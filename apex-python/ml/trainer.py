"""ML model trainer with leakage-aware time-series cross-validation.

Supports scikit-learn estimators and XGBoost.  Models are persisted with
:mod:`joblib` alongside a companion ``<model_id>.json`` metadata file (and a ``<model_id>.log``
with the captured training output) so that the Rust core can discover registered models without
importing Python.

Evaluation notes
----------------
* Folds are expanding-window :class:`~sklearn.model_selection.TimeSeriesSplit` folds over distinct
  *time steps*, so a dataset with several symbols is split by date, not by symbol.
* Labels computed from a forward return over ``horizon_bars`` bars overlap in time.  Every fold
  therefore drops (embargoes) training rows whose label window reaches into the test period, so
  nothing the model trains on was decided by test-period prices.
* When the data carries a forward-return column, each fold is also scored as a trading rule:
  using rows spaced ``horizon_bars`` apart so a return is never counted twice.  ``trade_mode``
  decides what a prediction means: ``long_flat`` (default) only ever holds longs, which is what an
  automation does unless shorting is switched on for it; ``long_short`` also shorts.  Class meaning
  follows the dataset's label kind: ``direction`` (0/1): 1 = long, 0 = short or flat;
  ``return_threshold`` (-1/0/1): 1 = long, -1 = short or flat, 0 = flat.  Without a label spec the
  second mapping is used, so 0/1 data is long/flat in both modes.
* ``holdout_pct`` removes the last share of the time steps before cross-validation (with an
  embargo of ``horizon_bars`` steps), fits the final model on the rest only, and scores the
  hold-out once, so that number never influenced any choice.
* Recipes (``algorithm="recipe"``) assemble library parts without code, see :mod:`ml.recipes`.
  Their stacker and calibrator are time-aware and are told how many rows a label reaches ahead
  (:meth:`ModelTrainer._embargo_rows`).  ``hyperparams`` must be empty for a recipe.
* Custom estimators (``algorithm="custom"``) are imported from a user file, see
  :mod:`ml.estimators`.  The file is copied next to the model so scoring keeps working if the
  original is edited or deleted.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import logging
import math
import platform
import sys
import time
from dataclasses import dataclass, field, fields
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

import joblib
import numpy as np
import polars as pl
from sklearn.base import BaseEstimator
from sklearn.metrics import accuracy_score, f1_score, precision_score, recall_score
from sklearn.model_selection import TimeSeriesSplit
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

from ml import catalog, recipes
from ml.catalog import DEFAULT_SEED, MAX_SEED, check_model_size, validate_hyperparams  # noqa: F401 - re-exported
from ml.estimators import (
    LabelEncodedClassifier,
    estimator_module_name,
    file_sha256,
    import_estimator_file,
    make_custom_estimator,
)

logger = logging.getLogger(__name__)


# ------------------------------------------------------------------
# Config / result types
# ------------------------------------------------------------------

@dataclass
class TrainingConfig:
    """Everything needed to kick off a training run."""

    algorithm: str  # see ml.catalog.supported_ids()
    data_path: str  # path to Parquet / CSV
    target_column: str
    feature_columns: list[str] = field(default_factory=list)
    hyperparams: dict[str, Any] = field(default_factory=dict)
    n_splits: int = 5
    output_dir: str = "models"
    lag_periods: list[int] = field(default_factory=lambda: [1, 5, 10])
    #: column with each row's forward return (a fraction); enables trading metrics
    return_column: str | None = None
    #: dataset name when the data came from the workbench
    dataset: str | None = None
    #: {"name", "symbols", "timeframe", "label", "custom_features", "context_symbols", "fee_pct"}
    #: copied from the dataset sidecar
    dataset_meta: dict[str, Any] | None = None
    #: bars covered by each label; sets the embargo and the trade spacing
    horizon_bars: int = 0
    #: who started the run: "user" | "copilot"
    source: str = "user"
    #: how the evaluation trades predictions: "long_flat" | "long_short"
    trade_mode: str = "long_flat"
    #: share (0-40) of the final time steps kept out of cross-validation and scored once
    holdout_pct: float = 0.0
    #: seed for estimators that take one; None means the fixed default (42)
    random_state: int | None = None
    #: custom estimator: file name (``my_model.py``) and the absolute path it is read from
    estimator_file: str | None = None
    estimator_path: str | None = None
    #: algorithm "recipe": the RecipeDto (steps + model) to build, and the saved recipe's name.
    #: The full spec is stored in the model's metadata so the model outlives the saved recipe.
    recipe: dict[str, Any] | None = None
    recipe_name: str | None = None
    #: audit ledger versions of what the model is built from, copied into its metadata:
    #: {"dataset_version", "dataset_csv_sha256", "estimator_file_version", "recipe_version"}.
    #: When "dataset_csv_sha256" is given, the data file must hash to it or the run is refused.
    provenance: dict[str, Any] | None = None


def _sha256_file(path: Path) -> str:
    """SHA-256 of a file, streamed."""
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


@dataclass
class TrainedModel:
    """Result of a successful training run."""

    model: BaseEstimator
    metrics: dict[str, float]
    feature_names: list[str]
    metadata_path: Path
    model_path: Path
    metadata: dict[str, Any] = field(default_factory=dict)
    log_path: Path | None = None


@dataclass
class Prepared:
    x: np.ndarray
    y: np.ndarray
    ret: np.ndarray | None
    #: time-step index of each row (0..U-1, equal for rows with the same timestamp)
    step: np.ndarray
    #: time step by which the row's label is fully decided
    label_end: np.ndarray
    #: symbol id of each row
    group: np.ndarray
    feature_names: list[str]
    n_steps: int


RECIPE = "recipe"
TRADE_MODES = ("long_flat", "long_short")
MAX_HOLDOUT_PCT = 40

_BARS_PER_YEAR = {"1m": 98_280, "5m": 19_656, "15m": 6_552, "1h": 1_638, "4h": 410, "1d": 252, "1w": 52}


def _clean(obj: Any) -> Any:
    """Make a structure JSON-safe: numpy scalars to Python, NaN/inf to None."""
    if isinstance(obj, dict):
        return {str(k): _clean(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_clean(v) for v in obj]
    if isinstance(obj, np.generic):
        obj = obj.item()
    if isinstance(obj, float) and not math.isfinite(obj):
        return None
    return obj


# ------------------------------------------------------------------
# Trainer
# ------------------------------------------------------------------


class ModelTrainer:
    """Train, evaluate, and register ML models for APEX strategies."""

    def train(self, config: TrainingConfig) -> TrainedModel:
        """End-to-end train → evaluate → register pipeline."""
        buffer = io.StringIO()
        handler = logging.StreamHandler(buffer)
        handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
        previous_level = logger.level
        logger.addHandler(handler)
        logger.setLevel(logging.INFO)
        try:
            return self._train(config, buffer)
        finally:
            logger.removeHandler(handler)
            logger.setLevel(previous_level)

    def _train(self, config: TrainingConfig, buffer: io.StringIO) -> TrainedModel:
        started = time.perf_counter()
        logger.info(
            "Starting training run: algorithm=%s source=%s trade_mode=%s holdout=%s%%",
            config.algorithm, config.source, config.trade_mode, config.holdout_pct,
        )
        recipe = self._prepare_recipe(config)
        hyperparams = {} if recipe is not None else validate_hyperparams(config.algorithm, config.hyperparams)
        if not 2 <= config.n_splits <= 20:
            raise ValueError("n_splits must be between 2 and 20")
        if config.trade_mode not in TRADE_MODES:
            raise ValueError(f"trade_mode must be one of {', '.join(TRADE_MODES)}")
        if not (isinstance(config.holdout_pct, (int, float)) and 0 <= config.holdout_pct <= MAX_HOLDOUT_PCT):
            raise ValueError(f"holdout_pct must be between 0 and {MAX_HOLDOUT_PCT}")
        if config.random_state is not None and not (
            isinstance(config.random_state, int) and not isinstance(config.random_state, bool)
            and 0 <= config.random_state <= MAX_SEED
        ):
            raise ValueError(f"random_state must be an integer from 0 to {MAX_SEED}")
        estimator = self._load_estimator_source(config)

        data_sha256 = self._verify_data_file(config)
        df = self._load_data(config)
        data = self._build_features(df, config)
        logger.info(
            "Loaded %d usable rows, %d features, %d time steps, embargo=%d bars",
            len(data.y), len(data.feature_names), data.n_steps, config.horizon_bars,
        )

        usable_rows = len(data.y)
        # Refuse a configuration that cannot fit in memory before spending time on it.
        if recipe is not None:
            recipes.check_model_size(recipe, usable_rows, int(len(set(data.y.tolist()))))
        else:
            check_model_size(config.algorithm, hyperparams, usable_rows, int(len(set(data.y.tolist()))))
        holdout_data = None
        split_info = None
        if config.holdout_pct > 0:
            data, holdout_data, split_info = self._split_holdout(data, config)
            logger.info(
                "Hold-out: last %d of %d time steps (%d rows) kept out; embargo %d steps; %d rows for cross-validation and the final fit",
                split_info["steps"], split_info["total_steps"], len(holdout_data.y), split_info["embargo_steps"], len(data.y),
            )

        classes = np.unique(data.y)
        if len(classes) < 2:
            raise ValueError(
                f"target '{config.target_column}' has only one class ({classes[0]}); "
                "a model needs at least two. Use a longer history, more symbols or a smaller threshold."
            )
        logger.info("Classes: %s", {int(c): int((data.y == c).sum()) for c in classes})
        label_kind = self._label_kind(config, np.unique(np.concatenate([data.y, holdout_data.y])) if holdout_data else classes)

        hyperparams = self._with_seed(config, hyperparams)
        if recipe is not None:
            factory = self._recipe_factory(recipe, hyperparams, self._embargo_rows(data))
        else:
            factory = self._model_factory(config, hyperparams, classes, estimator)
        folds, metrics, trading_periods = self._evaluate(factory, data, config, label_kind)

        model = factory()
        model.fit(data.x, data.y)
        logger.info("Final model fitted on %d rows", len(data.y))

        holdout = None
        if holdout_data is not None:
            holdout = self._score_holdout(model, holdout_data, config, label_kind, split_info)

        trading = self._summarise_trading(trading_periods, config)
        importances = (
            recipes.importances(model, data.feature_names, data.x) if recipe is not None
            else self._importances(model, data.feature_names)
        )
        duration = round(time.perf_counter() - started, 2)
        return self._register(
            model, data, metrics, folds, trading, importances, hyperparams, duration, config, buffer,
            holdout=holdout, estimator=estimator, total_rows=usable_rows, recipe=recipe,
            data_sha256=data_sha256,
        )

    # ------------------------------------------------------------------
    # Estimator construction
    # ------------------------------------------------------------------

    @staticmethod
    def _with_seed(config: TrainingConfig, hyperparams: dict[str, Any]) -> dict[str, Any]:
        """Fill in ``random_state`` for estimators that take one, so runs repeat exactly."""
        out = dict(hyperparams)
        seed = DEFAULT_SEED if config.random_state is None else int(config.random_state)
        if config.algorithm == RECIPE:
            out["random_state"] = seed  # recorded in the metadata; the recipe's members take it as their default
        elif config.algorithm == catalog.CUSTOM:
            out.setdefault("random_state", seed)
        elif "random_state" in catalog.param_specs(config.algorithm):
            out.setdefault("random_state", seed)
        return out

    def _load_estimator_source(self, config: TrainingConfig) -> dict[str, Any] | None:
        """Read the custom estimator file once; its bytes and hash are what gets used and saved."""
        if config.algorithm != catalog.CUSTOM:
            return None
        if not config.estimator_path or not config.estimator_file:
            raise ValueError("algorithm 'custom' needs an estimator_file (save one in the Train tab first)")
        path = Path(config.estimator_path)
        if not path.is_file():
            raise FileNotFoundError(f"estimator file not found: {config.estimator_file}")
        data = path.read_bytes()
        sha = file_sha256(data)
        module_name = estimator_module_name(sha)
        module = import_estimator_file(path, module_name)
        if file_sha256(path.read_bytes()) != sha:
            raise ValueError(f"{config.estimator_file} changed while it was being loaded; run the training again")
        return {"name": config.estimator_file, "path": str(path), "bytes": data, "sha256": sha,
                "module_name": module_name, "module": module}

    @staticmethod
    def _prepare_recipe(config: TrainingConfig) -> dict[str, Any] | None:
        """The checked, normalised recipe for ``algorithm="recipe"``; None for every other algorithm."""
        if config.algorithm != RECIPE:
            if config.recipe is not None:
                raise ValueError("a recipe can only be used with algorithm 'recipe'")
            return None
        if not isinstance(config.recipe, dict):
            raise ValueError("algorithm 'recipe' needs a recipe (build one in the Train tab first)")
        if config.hyperparams:
            raise ValueError("hyperparams must be empty for a recipe; set each model's parameters inside the recipe")
        spec = recipes.prepare(config.recipe)  # the trainer accepts an unsaved draft without a name
        if config.recipe_name:
            spec["name"] = str(config.recipe_name)
        return spec

    @staticmethod
    def _embargo_rows(data: Prepared) -> int:
        """How many rows ahead (in the chronological row order) a row's label reaches.

        Rows are sorted by time, then symbol, so with several symbols the rows of one time step are
        next to each other and a label that is ``horizon`` bars long reaches ``horizon`` *steps*
        ahead, which is about ``horizon * symbols`` rows, and more when a symbol has gaps. Rather
        than guess, count it: for every row, the rows after it whose time step is at or before the
        step where its label is decided, and take the maximum. A time-aware stacker or calibrator
        that leaves this many rows between its training rows and the rows it predicts never
        trains on a label that overlaps them. The count over the full data is an upper bound for
        any fold, because fold training rows are a subset in the same order.
        """
        n = len(data.step)
        if n == 0:
            return 0
        reach = np.searchsorted(data.step, data.label_end, side="right") - 1 - np.arange(n)
        return int(max(int(reach.max()), 0))

    @staticmethod
    def _recipe_factory(recipe: dict[str, Any], hyperparams: dict[str, Any], embargo_rows: int) -> Callable[[], BaseEstimator]:
        """Builder of fresh, unfitted recipe models (one per fold, one for the end)."""
        seed = hyperparams.get("random_state", DEFAULT_SEED)
        return lambda: recipes.build(recipe, embargo_rows=embargo_rows, random_state=seed)

    def _model_factory(
        self, config: TrainingConfig, hyperparams: dict[str, Any], classes: np.ndarray, estimator: dict[str, Any] | None
    ) -> Callable[[], BaseEstimator]:
        """Return a function that builds a fresh, unfitted model (one per fold, one for the end)."""
        algorithm = config.algorithm
        if algorithm == catalog.CUSTOM:
            module = estimator["module"]
            return lambda: make_custom_estimator(module, hyperparams)
        cls = catalog.estimator_class(algorithm)
        params = dict(hyperparams)
        if algorithm == "xgboost":
            params.setdefault("n_jobs", 1)
            params.setdefault("verbosity", 0)
            contiguous = list(classes) == list(range(len(classes)))
            return lambda: cls(**params) if contiguous else LabelEncodedClassifier(cls(**params))
        if algorithm == "logistic_regression":
            # Features are on very different scales; the scaler is part of the saved model.
            return lambda: make_pipeline(StandardScaler(), cls(**params))
        return lambda: cls(**params)

    # ------------------------------------------------------------------
    # Pipeline stages
    # ------------------------------------------------------------------

    @staticmethod
    def _verify_data_file(config: TrainingConfig) -> str | None:
        """Hash the data file and, when the audit ledger's hash was passed, require it to match.

        Dataset files are write-once, so a mismatch means the file is not the version the ledger
        recorded. The hash and the versions go into the run log and the model's metadata.
        """
        prov = config.provenance or {}
        path = Path(config.data_path)
        sha = _sha256_file(path) if path.is_file() else None
        expected = prov.get("dataset_csv_sha256")
        if expected and sha != expected:
            raise ValueError(
                f"dataset file {config.data_path} does not match the audit ledger "
                f"(expected sha256 {expected}, found {sha}); it was changed after it was recorded"
            )
        if config.dataset or prov.get("dataset_version") is not None:
            logger.info(
                "Dataset %s version %s, data file sha256 %s",
                config.dataset or "-", prov.get("dataset_version", "-"), sha,
            )
        if prov.get("estimator_file_version") is not None:
            logger.info("Custom estimator %s version %s", config.estimator_file, prov["estimator_file_version"])
        if prov.get("recipe_version") is not None:
            logger.info("Recipe %s version %s", config.recipe_name, prov["recipe_version"])
        return sha

    def _load_data(self, config: TrainingConfig) -> pl.DataFrame:
        """Load data from Parquet or CSV into a Polars DataFrame."""
        path = Path(config.data_path)
        if not path.exists():
            raise FileNotFoundError(f"Training data file not found: {path}")
        if path.suffix == ".parquet":
            return pl.read_parquet(path)
        if path.suffix == ".csv":
            return pl.read_csv(path, infer_schema_length=10_000)
        raise ValueError(f"Unsupported file format: {path.suffix}")

    def _build_features(self, df: pl.DataFrame, config: TrainingConfig) -> Prepared:
        """Apply lag features, drop unusable rows and return arrays plus time bookkeeping."""
        target = config.target_column
        ret_col = config.return_column
        meta_cols = {"time", "symbol"}
        if target not in df.columns:
            raise ValueError(f"target column '{target}' not found in {config.data_path}; columns: {df.columns}")
        if ret_col and ret_col not in df.columns:
            raise ValueError(f"return column '{ret_col}' not found in {config.data_path}; columns: {df.columns}")

        base = list(config.feature_columns) or [
            c for c in df.columns if c not in {target, ret_col} | meta_cols
        ]
        missing = [c for c in base if c not in df.columns]
        if missing:
            raise ValueError(
                f"feature columns not found in {config.data_path}: {', '.join(missing)} "
                f"(available: {', '.join(df.columns)})"
            )
        if not base:
            raise ValueError("no feature columns to train on")
        for col in base:
            if not df.schema[col].is_numeric():
                raise ValueError(f"feature column '{col}' is not numeric ({df.schema[col]})")
        if not df.schema[target].is_numeric():
            raise ValueError(f"target column '{target}' must be numeric class labels")

        # Chronological order (then symbol) so lags and folds follow time.
        order = [c for c in ("time", "symbol") if c in df.columns]
        if order:
            df = df.sort(order)
        has_symbol = "symbol" in df.columns

        feature_cols = list(base)
        for col in base:
            for lag in config.lag_periods:
                lag_name = f"{col}_lag{lag}"
                shifted = pl.col(col).shift(int(lag))
                if has_symbol:
                    shifted = shifted.over("symbol")
                df = df.with_columns(shifted.alias(lag_name))
                feature_cols.append(lag_name)

        x_all = df.select(feature_cols).cast(pl.Float64).to_numpy()
        y_all = df.select(target).cast(pl.Float64).to_numpy().ravel()
        keep = np.isfinite(x_all).all(axis=1) & np.isfinite(y_all)
        ret_all = None
        if ret_col:
            ret_all = df.select(ret_col).cast(pl.Float64).to_numpy().ravel()
            keep &= np.isfinite(ret_all)
        if not keep.any():
            raise ValueError("no usable rows: every row has a missing or non-finite value")
        if (y_all[keep] != np.round(y_all[keep])).any():
            raise ValueError(f"target column '{target}' must hold class labels (integers), not continuous values")

        x = x_all[keep]
        y = np.round(y_all[keep]).astype(int)
        ret = ret_all[keep] if ret_all is not None else None

        if "time" in df.columns:
            times = np.array(df.select(pl.col("time").cast(pl.Utf8)).to_series().to_list())[keep]
            _, step = np.unique(times, return_inverse=True)
            step = step.astype(int).ravel()
        else:
            step = np.arange(len(y))
        n_steps = int(step.max()) + 1
        if has_symbol:
            symbols = np.array(df.select(pl.col("symbol").cast(pl.Utf8)).to_series().to_list())[keep]
            _, group = np.unique(symbols, return_inverse=True)
            group = group.astype(int).ravel()
        else:
            group = np.zeros(len(y), dtype=int)

        label_end = self._label_end(step, group, max(int(config.horizon_bars), 0))
        return Prepared(x, y, ret, step, label_end, group, feature_cols, n_steps)

    @staticmethod
    def _label_end(step: np.ndarray, group: np.ndarray, horizon: int) -> np.ndarray:
        """Time step at which each row's forward-looking label is fully decided.

        Rows are chronological, so inside one symbol the label of row ``k`` is decided at the time
        step of row ``k + horizon``.  The last ``horizon`` rows of a symbol use ``step + horizon``.
        """
        end = step + horizon
        if horizon == 0:
            return end
        for g in np.unique(group):
            idx = np.flatnonzero(group == g)
            if len(idx) > horizon:
                end[idx[:-horizon]] = step[idx[horizon:]]
        return end

    def _folds(self, data: Prepared, config: TrainingConfig):
        """Yield (train_idx, test_idx) with the embargo applied."""
        horizon = max(int(config.horizon_bars), 0)
        steps = np.arange(data.n_steps)
        test_size = data.n_steps // (config.n_splits + 1)
        if test_size < 1 or data.n_steps - horizon - test_size * config.n_splits <= 0:
            raise ValueError(
                f"not enough data for n_splits={config.n_splits} with a {horizon}-bar embargo: "
                f"{len(data.y)} rows over {data.n_steps} time steps. Lower n_splits or build a larger dataset."
            )
        cv = TimeSeriesSplit(n_splits=config.n_splits, gap=horizon)
        for k, (_, test_steps) in enumerate(cv.split(steps), start=1):
            test_start = int(test_steps[0])
            test_mask = (data.step >= test_start) & (data.step <= int(test_steps[-1]))
            # Purge: training rows must have their label decided before the test period starts.
            train_mask = (data.step < test_start) & (data.label_end < test_start)
            train_idx, test_idx = np.flatnonzero(train_mask), np.flatnonzero(test_mask)
            if len(train_idx) == 0 or len(test_idx) == 0:
                raise ValueError(
                    f"fold {k} has no {'training' if not len(train_idx) else 'test'} rows after the "
                    f"{horizon}-bar embargo; lower n_splits or build a larger dataset"
                )
            yield train_idx, test_idx

    def _evaluate(
        self, factory: Callable[[], BaseEstimator], data: Prepared, config: TrainingConfig, label_kind: str = "return_threshold"
    ):
        """Run the embargoed time-series CV; return per-fold records, mean metrics, trade periods."""
        records: list[dict[str, Any]] = []
        periods: list[dict[str, Any]] = []
        horizon = max(int(config.horizon_bars), 1)
        can_trade = data.ret is not None and set(np.unique(data.y)) <= {-1, 0, 1}

        for fold, (train_idx, test_idx) in enumerate(self._folds(data, config), start=1):
            if len(np.unique(data.y[train_idx])) < 2:
                raise ValueError(
                    f"fold {fold} training rows contain a single class; "
                    "use a longer history, more symbols or fewer folds"
                )
            fold_model = factory()
            fold_model.fit(data.x[train_idx], data.y[train_idx])
            preds = fold_model.predict(data.x[test_idx])
            y_test = data.y[test_idx]

            record: dict[str, Any] = {
                "fold": fold,
                "train_rows": int(len(train_idx)),
                "test_rows": int(len(test_idx)),
                "accuracy": float(accuracy_score(y_test, preds)),
                "f1": float(f1_score(y_test, preds, average="weighted", zero_division=0)),
                "precision": float(precision_score(y_test, preds, average="weighted", zero_division=0)),
                "recall": float(recall_score(y_test, preds, average="weighted", zero_division=0)),
                "baseline_accuracy": float(np.bincount(y_test - y_test.min()).max() / len(y_test)),
                "trade_return_pct": None,
                "hit_rate": None,
                "trades": None,
            }
            if can_trade:
                trade = self._trade_fold(
                    preds, data.ret[test_idx], data.step[test_idx], data.group[test_idx], horizon,
                    kind=label_kind, trade_mode=config.trade_mode, fee=self._fee(config),
                )
                periods.append(trade)
                record["trade_return_pct"] = trade["return_pct"]
                record["hit_rate"] = trade["hit_rate"]
                record["trades"] = trade["trades"]
            logger.info(
                "Fold %d - train=%d test=%d accuracy=%.4f baseline=%.4f f1=%.4f precision=%.4f recall=%.4f%s",
                fold, record["train_rows"], record["test_rows"], record["accuracy"],
                record["baseline_accuracy"], record["f1"], record["precision"], record["recall"],
                "" if record["trade_return_pct"] is None
                else f" trade_return={record['trade_return_pct']:.2f}% trades={record['trades']}",
            )
            records.append(record)

        metrics = {
            key: float(np.mean([r[key] for r in records]))
            for key in ("accuracy", "f1", "precision", "recall", "baseline_accuracy")
        }
        return records, metrics, periods

    @staticmethod
    def _label_kind(config: TrainingConfig, classes: np.ndarray) -> str:
        """"direction" or "return_threshold": which meaning the classes have.

        Read from the dataset's label spec; without one, inferred from the classes. Data with no
        -1 class and no label spec is treated like ``return_threshold`` (0 = flat), which is how
        automations read a legacy model.
        """
        kind = ((config.dataset_meta or {}).get("label") or {}).get("kind")
        if kind in ("direction", "return_threshold"):
            return kind
        return "return_threshold"

    @staticmethod
    def _fee(config: TrainingConfig) -> float:
        """Round-trip cost as a fraction (the dataset's ``fwd_ret`` already has it subtracted)."""
        try:
            fee = float((config.dataset_meta or {}).get("fee_pct") or 0.0)
        except (TypeError, ValueError):
            return 0.0
        return max(fee, 0.0) / 100.0 if math.isfinite(fee) else 0.0

    @staticmethod
    def _positions(preds: np.ndarray, kind: str, trade_mode: str) -> np.ndarray:
        """Position taken for each prediction: 1 long, -1 short, 0 flat.

        ``direction``: 1 = long, 0 = short (``long_short``) or flat. ``return_threshold``:
        1 = long, -1 = short (``long_short``) or flat, 0 = flat. Any other class is flat.
        """
        preds = np.asarray(preds)
        pos = np.zeros(len(preds), dtype=float)
        pos[preds == 1] = 1.0
        if trade_mode == "long_short":
            pos[preds == (0 if kind == "direction" else -1)] = -1.0
        return pos

    @staticmethod
    def _trade_fold(
        preds: np.ndarray,
        ret: np.ndarray,
        step: np.ndarray,
        group: np.ndarray,
        horizon: int,
        kind: str = "return_threshold",
        trade_mode: str = "long_flat",
        fee: float = 0.0,
    ) -> dict[str, Any]:
        """Trade the predicted signals on rows spaced ``horizon`` apart on a shared time grid.

        Time steps are bucketed into windows of ``horizon`` steps starting at the fold's first step.
        Each symbol trades its first row in every window, so a slot is one point in time for all
        symbols, whatever their calendars or start dates. The slot's return is the equal weight
        average of the per-symbol results over the symbols present. A long earns ``ret``; a short
        earns ``-ret - 2 * fee`` (``ret`` already has the round-trip fee taken off, which would
        otherwise turn the fee into income for a short); a flat row earns nothing.
        """
        positions = ModelTrainer._positions(preds, kind, trade_mode)
        slots: dict[int, list[tuple[float, float, bool]]] = {}
        if len(step):
            origin = int(np.min(step))
            for g in np.unique(group):
                idx = np.flatnonzero(group == g)
                idx = idx[np.argsort(step[idx], kind="stable")]
                seen: set[int] = set()
                for i in idx:
                    slot = (int(step[i]) - origin) // horizon
                    if slot in seen:
                        continue
                    seen.add(slot)
                    r = float(ret[i])
                    pnl = r if positions[i] > 0 else (-r - 2.0 * fee if positions[i] < 0 else 0.0)
                    slots.setdefault(slot, []).append((pnl, r, bool(positions[i] != 0)))
        strategy, hold, wins, trades = [], [], 0, 0
        for s in sorted(slots):
            pairs = slots[s]
            strategy.append(float(np.mean([p for p, _, _ in pairs])))
            hold.append(float(np.mean([r for _, r, _ in pairs])))
            for pnl, _, traded in pairs:
                if traded:
                    trades += 1
                    wins += 1 if pnl > 0 else 0
        growth = float(np.prod(1 + np.array(strategy))) if strategy else 1.0
        return {
            "return_pct": (growth - 1) * 100,
            "hit_rate": (wins / trades) if trades else None,
            "trades": trades,
            "wins": wins,
            "strategy": strategy,
            "hold": hold,
        }

    # ------------------------------------------------------------------
    # Hold-out
    # ------------------------------------------------------------------

    @staticmethod
    def _subset(data: Prepared, mask: np.ndarray, n_steps: int) -> Prepared:
        return Prepared(
            data.x[mask], data.y[mask], None if data.ret is None else data.ret[mask], data.step[mask],
            data.label_end[mask], data.group[mask], data.feature_names, n_steps,
        )

    def _split_holdout(self, data: Prepared, config: TrainingConfig):
        """Split off the last ``holdout_pct`` percent of distinct time steps.

        Returns ``(fit_data, holdout_data, info)``. ``fit_data`` (used for cross-validation and for
        the final fit) keeps only rows whose time step lies at least ``horizon`` steps before the
        hold-out and whose label is decided before the hold-out starts, so no label window reaches
        into it.
        """
        horizon = max(int(config.horizon_bars), 0)
        total = data.n_steps
        steps = max(1, int(round(total * float(config.holdout_pct) / 100.0)))
        hold_start = total - steps
        cv_end = hold_start - horizon
        fit_mask = (data.step < cv_end) & (data.label_end < hold_start)
        hold_mask = data.step >= hold_start
        if cv_end < 2 or not fit_mask.any() or not hold_mask.any():
            raise ValueError(
                f"a {config.holdout_pct:g}% hold-out of {total} time steps with a {horizon}-bar embargo leaves too little "
                "data; use a smaller hold-out or build a larger dataset"
            )
        info = {
            "pct": float(config.holdout_pct), "steps": int(steps), "total_steps": int(total),
            "start_step": int(hold_start), "embargo_steps": int(horizon),
        }
        return self._subset(data, fit_mask, int(cv_end)), self._subset(data, hold_mask, int(total)), info

    def _score_holdout(
        self, model: BaseEstimator, hold: Prepared, config: TrainingConfig, label_kind: str, info: dict[str, Any]
    ) -> dict[str, Any]:
        """Score the final model once on the hold-out rows."""
        preds = np.asarray(model.predict(hold.x))
        record: dict[str, Any] = {
            "rows": int(len(hold.y)),
            "accuracy": float(accuracy_score(hold.y, preds)),
            "baseline_accuracy": float(np.bincount(hold.y - hold.y.min()).max() / len(hold.y)),
            "trading": None,
            **info,
        }
        if hold.ret is not None and set(np.unique(hold.y)) <= {-1, 0, 1}:
            trade = self._trade_fold(
                preds, hold.ret, hold.step, hold.group, max(int(config.horizon_bars), 1),
                kind=label_kind, trade_mode=config.trade_mode, fee=self._fee(config),
            )
            record["trading"] = self._summarise_trading([trade], config)
        logger.info(
            "Hold-out (%d rows): accuracy=%.4f baseline=%.4f%s", record["rows"], record["accuracy"], record["baseline_accuracy"],
            "" if record["trading"] is None else f" trade_return={record['trading']['strategy_return_pct']:.2f}% trades={record['trading']['trades']}",
        )
        return record

    @staticmethod
    def _summarise_trading(periods: list[dict[str, Any]], config: TrainingConfig) -> dict[str, float] | None:
        """Aggregate the fold results (folds are disjoint and in time order)."""
        if not periods:
            return None
        strategy = np.array([r for p in periods for r in p["strategy"]], dtype=float)
        hold = np.array([r for p in periods for r in p["hold"]], dtype=float)
        if len(strategy) == 0:
            return None
        trades = sum(p["trades"] for p in periods)
        wins = sum(p["wins"] for p in periods)
        equity = np.cumprod(1 + strategy)
        peak = np.maximum.accumulate(np.concatenate([[1.0], equity]))[1:]
        drawdown = float(((equity / peak) - 1).min()) * 100
        timeframe = (config.dataset_meta or {}).get("timeframe") or "1d"
        per_year = _BARS_PER_YEAR.get(timeframe, 252) / max(int(config.horizon_bars), 1)
        sd = float(strategy.std(ddof=1)) if len(strategy) > 1 else 0.0
        sharpe = float(strategy.mean() / sd * math.sqrt(per_year)) if sd > 1e-12 else 0.0
        return {
            "strategy_return_pct": float(equity[-1] - 1) * 100,
            "buy_hold_return_pct": float(np.prod(1 + hold) - 1) * 100,
            "hit_rate": float(wins / trades) if trades else 0.0,
            "trades": int(trades),
            "max_drawdown_pct": drawdown,
            "sharpe": sharpe,
        }

    @staticmethod
    def _importances(model: Any, names: list[str]) -> list[dict[str, Any]]:
        """Normalised feature importances (impurity based, or |coefficient| for linear models)."""
        est = model
        if hasattr(est, "steps"):  # Pipeline
            est = est.steps[-1][1]
        raw = None
        try:
            if hasattr(est, "feature_importances_"):
                raw = np.asarray(est.feature_importances_, dtype=float)
            elif hasattr(est, "coef_"):
                raw = np.abs(np.asarray(est.coef_, dtype=float)).reshape(-1, len(names)).mean(axis=0)
        except Exception:  # noqa: BLE001 - importances are optional
            raw = None
        if raw is None or raw.shape != (len(names),) or not np.isfinite(raw).all() or raw.sum() <= 0:
            return []
        raw = raw / raw.sum()
        ranked = sorted(zip(names, raw), key=lambda kv: -kv[1])
        return [{"feature": n, "importance": float(v)} for n, v in ranked]

    @staticmethod
    def _library_versions() -> dict[str, str]:
        import sklearn  # noqa: PLC0415

        versions = {
            "python": platform.python_version(),
            "scikit_learn": sklearn.__version__,
            "polars": pl.__version__,
            "joblib": joblib.__version__,
        }
        try:
            import xgboost  # noqa: PLC0415

            versions["xgboost"] = xgboost.__version__
        except Exception:  # noqa: BLE001 - optional dependency
            pass
        return versions

    def _register(
        self,
        model: BaseEstimator,
        data: Prepared,
        metrics: dict[str, float],
        folds: list[dict[str, Any]],
        trading: dict[str, float] | None,
        importances: list[dict[str, Any]],
        hyperparams: dict[str, Any],
        duration: float,
        config: TrainingConfig,
        buffer: io.StringIO,
        holdout: dict[str, Any] | None = None,
        estimator: dict[str, Any] | None = None,
        total_rows: int | None = None,
        recipe: dict[str, Any] | None = None,
        data_sha256: str | None = None,
    ) -> TrainedModel:
        """Persist model with joblib + companion metadata json and log."""
        out_dir = Path(config.output_dir)
        out_dir.mkdir(parents=True, exist_ok=True)

        timestamp = datetime.now(tz=timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        model_id = f"{config.algorithm}_{timestamp}"
        suffix = 1
        while True:
            model_filename = f"{model_id}.joblib"
            model_path = out_dir / model_filename
            # An id that was used by a deleted (archived) model is never handed out again.
            archived = any((out_dir / "archive").glob(f"{model_id}@v*")) if (out_dir / "archive").is_dir() else False
            if not (out_dir / f"{model_id}.json").exists() and not archived:
                try:
                    # Exclusive creation claims the id atomically: a concurrent training that
                    # picked the same id gets FileExistsError instead of overwriting this model.
                    with open(model_path, "xb") as handle:
                        joblib.dump(model, handle)
                    break
                except FileExistsError:
                    pass
            suffix += 1
            model_id = f"{config.algorithm}_{timestamp}_{suffix}"

        logger.info("Model saved to %s", model_path)
        artifact_sha256 = _sha256_file(model_path)

        estimator_meta: dict[str, Any] = {}
        if estimator is not None:
            # Keep the exact code the model was trained with next to it.
            copy_name = f"{model_id}_estimator.py"
            try:
                (out_dir / copy_name).write_bytes(estimator["bytes"])
            except OSError:
                model_path.unlink(missing_ok=True)
                raise
            estimator_meta = {
                "estimator_file": estimator["name"],
                "estimator_path": config.estimator_path,
                "estimator_sha256": estimator["sha256"],
                "estimator_copy": copy_name,
                "estimator_module": estimator["module_name"],
            }
            logger.info("Estimator file copied to %s (sha256 %s)", copy_name, estimator["sha256"][:12])

        meta = config.dataset_meta or {}
        effective = dict(hyperparams)
        seed_used = effective.get("random_state")
        if recipe is not None:
            effective.pop("random_state", None)  # a recipe has no top-level hyperparameters
        metadata = {
            "model_id": model_id,
            "algorithm": config.algorithm,
            "hyperparams": effective,
            "feature_names": data.feature_names,
            "target_column": config.target_column,
            "data_path": config.data_path,
            "n_splits": config.n_splits,
            "metrics": metrics,
            "model_file": model_filename,
            "created_utc": timestamp,
            # Added by the workbench; every one of these is optional for readers.
            "display_name": "",
            "notes": "",
            "tags": [],
            "source": config.source if config.source in ("user", "copilot") else "user",
            "lag_periods": [int(v) for v in config.lag_periods],
            "return_column": config.return_column,
            "horizon_bars": int(config.horizon_bars),
            "dataset": config.dataset or meta.get("name"),
            "symbols": list(meta.get("symbols") or []),
            "timeframe": meta.get("timeframe"),
            "label": meta.get("label"),
            "rows": int(total_rows if total_rows is not None else len(data.y)),
            "duration_secs": duration,
            "library_versions": self._library_versions(),
            "cv_folds": folds,
            "feature_importance": importances,
            "trading": trading,
            "trade_mode": config.trade_mode,
            "holdout": holdout,
            "random_state": seed_used,
            # Copied from the dataset so inference can recompute the same features.
            "custom_features": list(meta.get("custom_features") or []),
            "context_symbols": list(meta.get("context_symbols") or []),
            "fee_pct": float(meta.get("fee_pct") or 0.0),
            **({"recipe": recipe, "recipe_name": recipe.get("name") or config.recipe_name} if recipe is not None else {}),
            **({"estimator_file": None} if estimator is None else {}),
            **estimator_meta,
            # Audit trail: the exact artifact and data, and the ledger versions of dataset, code and recipe.
            "artifact_sha256": artifact_sha256,
            "data_file_sha256": data_sha256,
            **{
                key: (config.provenance or {})[key]
                for key in ("dataset_version", "dataset_csv_sha256", "estimator_file_version", "recipe_version")
                if key in (config.provenance or {})
            },
        }
        metadata_path = out_dir / f"{model_id}.json"
        metadata_path.write_text(json.dumps(_clean(metadata), indent=2, allow_nan=False))
        logger.info("Metadata saved to %s", metadata_path)

        log_path = out_dir / f"{model_id}.log"
        log_path.write_text(buffer.getvalue())

        return TrainedModel(
            model=model,
            metrics=metrics,
            feature_names=data.feature_names,
            metadata_path=metadata_path,
            model_path=model_path,
            metadata=metadata,
            log_path=log_path,
        )


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Train an APEX ML model")
    parser.add_argument(
        "--config-json",
        required=True,
        help="JSON-encoded TrainingConfig payload",
    )
    return parser.parse_args()


def _config_from_json(raw: dict[str, Any]) -> TrainingConfig:
    known = {f.name for f in fields(TrainingConfig)}
    unknown = sorted(set(raw) - known)
    if unknown:
        logger.warning("Ignoring unknown training options: %s", ", ".join(unknown))
    return TrainingConfig(**{k: v for k, v in raw.items() if k in known})


def main() -> int:
    logging.basicConfig(level=logging.INFO)
    args = _parse_args()

    try:
        config = _config_from_json(json.loads(args.config_json))
        trained = ModelTrainer().train(config)

        payload = {
            "model_id": trained.model_path.stem,
            "algorithm": config.algorithm,
            "metrics": trained.metrics,
            "feature_names": trained.feature_names,
            "created_at": trained.metadata["created_utc"],
            "model_path": str(trained.model_path),
            "metadata_path": str(trained.metadata_path),
            "data_path": config.data_path,
            "target_column": config.target_column,
            "duration_secs": trained.metadata["duration_secs"],
        }
        print(json.dumps(_clean(payload), allow_nan=False))
        return 0
    except Exception as exc:  # pragma: no cover - surfaced via stderr to Rust
        logger.exception("Training failed")
        # The Rust side shows the last non-empty stderr line to the user.
        print(f"Training failed: {exc}", file=sys.stderr, flush=True)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
