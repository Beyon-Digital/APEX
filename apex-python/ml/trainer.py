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
  1 = long, anything else = flat, using rows spaced ``horizon_bars`` apart so a return is never
  counted twice.  The rule never shorts, because APEX automations are long/flat only: a number
  that included short profits would be one the automation could not earn.
"""

from __future__ import annotations

import argparse
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
from sklearn.base import BaseEstimator, clone
from sklearn.ensemble import GradientBoostingClassifier, RandomForestClassifier
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import accuracy_score, f1_score, precision_score, recall_score
from sklearn.model_selection import TimeSeriesSplit
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

from ml.estimators import LabelEncodedClassifier

logger = logging.getLogger(__name__)


# ------------------------------------------------------------------
# Hyperparameter allow-list
# ------------------------------------------------------------------

_MAX_SEED = 2**31 - 1


def _is_number(v: Any) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def _int_in(lo: int, hi: int, allow_none: bool = False) -> tuple[str, Callable[[Any], Any]]:
    desc = f"an integer from {lo} to {hi}" + (" or null" if allow_none else "")

    def check(v: Any) -> Any:
        if v is None and allow_none:
            return None
        if _is_number(v) and float(v).is_integer() and lo <= v <= hi:
            return int(v)
        raise ValueError

    return desc, check


def _float_in(hi: float) -> tuple[str, Callable[[Any], Any]]:
    desc = f"a number greater than 0 and at most {hi:g}"

    def check(v: Any) -> Any:
        if _is_number(v) and 0 < v <= hi:
            return float(v)
        raise ValueError

    return desc, check


def _choice(*options: Any) -> tuple[str, Callable[[Any], Any]]:
    desc = "one of " + ", ".join("null" if o is None else repr(o) for o in options)

    def check(v: Any) -> Any:
        if v in options and not isinstance(v, bool):
            return v
        raise ValueError

    return desc, check


def _max_features() -> tuple[str, Callable[[Any], Any]]:
    desc = "'sqrt', 'log2', null, an integer from 1 to 1000 or a fraction in (0, 1]"

    def check(v: Any) -> Any:
        if v is None or v in ("sqrt", "log2"):
            return v
        if isinstance(v, bool):
            raise ValueError
        if isinstance(v, int) and 1 <= v <= 1000:
            return v
        if isinstance(v, float) and 0 < v <= 1:
            return v
        raise ValueError

    return desc, check


HYPERPARAMS: dict[str, dict[str, tuple[str, Callable[[Any], Any]]]] = {
    "random_forest": {
        "n_estimators": _int_in(1, 2000),
        "max_depth": _int_in(1, 100, allow_none=True),
        "min_samples_leaf": _int_in(1, 1000),
        "max_features": _max_features(),
        "class_weight": _choice(None, "balanced", "balanced_subsample"),
        "random_state": _int_in(0, _MAX_SEED),
    },
    "gradient_boosting": {
        "n_estimators": _int_in(1, 2000),
        "learning_rate": _float_in(1.0),
        "max_depth": _int_in(1, 20),
        "subsample": _float_in(1.0),
        "random_state": _int_in(0, _MAX_SEED),
    },
    "logistic_regression": {
        "C": _float_in(1_000_000.0),
        "max_iter": _int_in(1, 100_000),
        "class_weight": _choice(None, "balanced"),
    },
    "xgboost": {
        "n_estimators": _int_in(1, 2000),
        "max_depth": _int_in(1, 20),
        "learning_rate": _float_in(1.0),
        "subsample": _float_in(1.0),
        "colsample_bytree": _float_in(1.0),
        "random_state": _int_in(0, _MAX_SEED),
    },
}

# Algorithms whose results should be reproducible unless the caller picks another seed.
_DEFAULT_SEED = 42


def validate_hyperparams(algorithm: str, hyperparams: dict[str, Any] | None) -> dict[str, Any]:
    """Return the checked (and normalised) hyperparameters, or raise ``ValueError``.

    Only keys on the algorithm's allow-list are accepted, each with a type and range check.
    """
    if algorithm not in HYPERPARAMS:
        raise ValueError(f"Unknown algorithm '{algorithm}'. Supported: {sorted(HYPERPARAMS)}")
    allowed = HYPERPARAMS[algorithm]
    out: dict[str, Any] = {}
    for key, value in (hyperparams or {}).items():
        if key not in allowed:
            raise ValueError(
                f"unknown hyperparameter '{key}' for {algorithm}; allowed: {', '.join(allowed)}"
            )
        desc, check = allowed[key]
        try:
            out[key] = check(value)
        except ValueError:
            raise ValueError(
                f"invalid value {value!r} for hyperparameter '{key}' of {algorithm}: expected {desc}"
            ) from None
    return out


# ------------------------------------------------------------------
# Config / result types
# ------------------------------------------------------------------

@dataclass
class TrainingConfig:
    """Everything needed to kick off a training run."""

    algorithm: str  # "random_forest" | "gradient_boosting" | "xgboost" | "logistic_regression"
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
    #: {"name", "symbols", "timeframe", "label"} copied from the dataset sidecar
    dataset_meta: dict[str, Any] | None = None
    #: bars covered by each label; sets the embargo and the trade spacing
    horizon_bars: int = 0
    #: who started the run: "user" | "copilot"
    source: str = "user"


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

    _ALGORITHMS: dict[str, type[BaseEstimator]] = {
        "random_forest": RandomForestClassifier,
        "gradient_boosting": GradientBoostingClassifier,
        "logistic_regression": LogisticRegression,
    }

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
        logger.info("Starting training run: algorithm=%s source=%s", config.algorithm, config.source)
        hyperparams = validate_hyperparams(config.algorithm, config.hyperparams)
        if not 2 <= config.n_splits <= 20:
            raise ValueError("n_splits must be between 2 and 20")

        df = self._load_data(config)
        data = self._build_features(df, config)
        logger.info(
            "Loaded %d usable rows, %d features, %d time steps, embargo=%d bars",
            len(data.y), len(data.feature_names), data.n_steps, config.horizon_bars,
        )
        classes = np.unique(data.y)
        if len(classes) < 2:
            raise ValueError(
                f"target '{config.target_column}' has only one class ({classes[0]}); "
                "a model needs at least two. Use a longer history, more symbols or a smaller threshold."
            )
        logger.info("Classes: %s", {int(c): int((data.y == c).sum()) for c in classes})

        model = self._build_model(config.algorithm, hyperparams, classes)
        folds, metrics, trading_periods = self._evaluate(model, data, config)

        model.fit(data.x, data.y)
        logger.info("Final model fitted on %d rows", len(data.y))

        trading = self._summarise_trading(trading_periods, config)
        importances = self._importances(model, data.feature_names)
        duration = round(time.perf_counter() - started, 2)
        return self._register(
            model, data, metrics, folds, trading, importances, hyperparams, duration, config, buffer
        )

    # ------------------------------------------------------------------
    # Pipeline stages
    # ------------------------------------------------------------------

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

    def _build_model(self, algorithm: str, hyperparams: dict[str, Any], classes: np.ndarray) -> BaseEstimator:
        """Instantiate the requested estimator."""
        params = dict(hyperparams)
        if algorithm == "xgboost":
            import xgboost as xgb  # noqa: PLC0415

            params.setdefault("random_state", _DEFAULT_SEED)
            contiguous = list(classes) == list(range(len(classes)))
            est = xgb.XGBClassifier(n_jobs=1, verbosity=0, **params)
            return est if contiguous else LabelEncodedClassifier(est)
        cls = self._ALGORITHMS.get(algorithm)
        if cls is None:
            raise ValueError(
                f"Unknown algorithm '{algorithm}'. "
                f"Supported: {sorted([*self._ALGORITHMS, 'xgboost'])}"
            )
        if algorithm == "logistic_regression":
            # Features are on very different scales; the scaler is part of the saved model.
            return make_pipeline(StandardScaler(), cls(**params))
        params.setdefault("random_state", _DEFAULT_SEED)
        return cls(**params)

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

    def _evaluate(self, model: BaseEstimator, data: Prepared, config: TrainingConfig):
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
            fold_model = clone(model)
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
                    preds, data.ret[test_idx], data.step[test_idx], data.group[test_idx], horizon
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
    def _trade_fold(
        preds: np.ndarray, ret: np.ndarray, step: np.ndarray, group: np.ndarray, horizon: int
    ) -> dict[str, Any]:
        """Trade the predicted signals on rows spaced ``horizon`` apart on a shared time grid.

        Time steps are bucketed into windows of ``horizon`` steps starting at the fold's first step.
        Each symbol trades its first row in every window, so a slot is one point in time for all
        symbols, whatever their calendars or start dates. The slot's return is the equal weight
        average of ``position * forward_return`` over the symbols present, where the position is 1
        (long) for a prediction of 1 and 0 (flat) for every other class.
        """
        slots: dict[int, list[tuple[float, float]]] = {}
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
                    # Long/flat: only class 1 holds a position. -1 ("down") exits, it never shorts.
                    slots.setdefault(slot, []).append((1.0 if preds[i] == 1 else 0.0, float(ret[i])))
        strategy, hold, wins, trades = [], [], 0, 0
        for s in sorted(slots):
            pairs = slots[s]
            strategy.append(float(np.mean([p * r for p, r in pairs])))
            hold.append(float(np.mean([r for _, r in pairs])))
            for p, r in pairs:
                if p != 0:
                    trades += 1
                    wins += 1 if p * r > 0 else 0
        growth = float(np.prod(1 + np.array(strategy))) if strategy else 1.0
        return {
            "return_pct": (growth - 1) * 100,
            "hit_rate": (wins / trades) if trades else None,
            "trades": trades,
            "wins": wins,
            "strategy": strategy,
            "hold": hold,
        }

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
            if not (out_dir / f"{model_id}.json").exists():
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

        meta = config.dataset_meta or {}
        effective = dict(hyperparams)
        if config.algorithm in ("random_forest", "gradient_boosting", "xgboost"):
            effective.setdefault("random_state", _DEFAULT_SEED)
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
            "rows": int(len(data.y)),
            "duration_secs": duration,
            "library_versions": self._library_versions(),
            "cv_folds": folds,
            "feature_importance": importances,
            "trading": trading,
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
