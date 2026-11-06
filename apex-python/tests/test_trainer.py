"""Tests for the ML trainer: metadata, per-fold records, embargo, hyperparameters, batch predict."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest

from ml.trainer import (
    ModelTrainer,
    TrainingConfig,
    _config_from_json,
    validate_hyperparams,
)

APEX_PYTHON = Path(__file__).resolve().parents[1]
FEATURES = ["ret_1", "rsi_14", "vol_10"]


def write_dataset(
    path: Path,
    steps: int = 240,
    symbols: tuple[str, ...] = ("AAA", "BBB"),
    kind: str = "direction",
    seed: int = 3,
) -> Path:
    """A synthetic labelled CSV shaped like the workbench's datasets.

    The label follows ret_1 (plus noise) so a model can learn something, and fwd_ret agrees with
    the label on average so the trading result is meaningful.
    """
    rng = np.random.default_rng(seed)
    rows = ["time,symbol," + ",".join(FEATURES) + ",fwd_ret,target"]
    days = np.arange(steps)
    for symbol in symbols:
        for d in days:
            ret_1, rsi, vol = rng.normal(0, 0.01), rng.uniform(20, 80), abs(rng.normal(0.01, 0.003))
            score = ret_1 * 100 + rng.normal(0, 0.7)
            fwd = 0.01 * np.tanh(score) + rng.normal(0, 0.004)
            if kind == "direction":
                target = 1 if fwd > 0 else 0
            else:
                target = 1 if fwd > 0.004 else (-1 if fwd < -0.004 else 0)
            date = (np.datetime64("2024-01-01") + np.timedelta64(int(d), "D")).astype(str)
            rows.append(f"{date}T00:00:00Z,{symbol},{ret_1},{rsi},{vol},{fwd},{target}")
    path.write_text("\n".join(rows) + "\n")
    return path


def make_config(tmp_path: Path, **overrides) -> TrainingConfig:
    data = overrides.pop("data_path", None) or write_dataset(tmp_path / "ds.csv")
    base = dict(
        algorithm="random_forest",
        data_path=str(data),
        target_column="target",
        feature_columns=list(FEATURES),
        hyperparams={"n_estimators": 20, "max_depth": 4},
        n_splits=4,
        output_dir=str(tmp_path / "models"),
        lag_periods=[],
        return_column="fwd_ret",
        dataset="ds",
        dataset_meta={
            "name": "ds",
            "symbols": ["AAA", "BBB"],
            "timeframe": "1d",
            "label": {"horizon_bars": 5, "kind": "direction", "threshold_pct": 0.0},
        },
        horizon_bars=5,
        source="copilot",
    )
    base.update(overrides)
    return TrainingConfig(**base)


@pytest.fixture()
def trained(tmp_path):
    config = make_config(tmp_path)
    return config, ModelTrainer().train(config)


def strict_json(path: Path) -> dict:
    def refuse(token):  # NaN / Infinity would break the Rust reader
        raise AssertionError(f"non-finite number {token} in {path.name}")

    return json.loads(path.read_text(), parse_constant=refuse)


# ------------------------------------------------------------------
# Metadata
# ------------------------------------------------------------------


def test_metadata_has_the_workbench_fields(trained):
    config, result = trained
    meta = strict_json(result.metadata_path)
    for key in (
        "model_id", "algorithm", "hyperparams", "feature_names", "target_column", "data_path", "n_splits",
        "metrics", "model_file", "created_utc",
    ):
        assert key in meta, key  # the fields the first version wrote must still be there
    assert meta["model_id"] == result.model_path.stem
    assert meta["feature_names"] == FEATURES
    assert meta["source"] == "copilot"
    assert meta["dataset"] == "ds"
    assert meta["symbols"] == ["AAA", "BBB"]
    assert meta["timeframe"] == "1d"
    assert meta["label"]["horizon_bars"] == 5
    assert meta["rows"] == 480 - 2 * 0  # nothing dropped: lag_periods is empty
    assert meta["duration_secs"] >= 0
    assert meta["display_name"] == "" and meta["notes"] == "" and meta["tags"] == []
    versions = meta["library_versions"]
    assert {"python", "scikit_learn", "polars", "joblib"} <= set(versions)
    assert meta["hyperparams"]["n_estimators"] == 20 and meta["hyperparams"]["random_state"] == 42
    for key in ("accuracy", "f1", "precision", "recall", "baseline_accuracy"):
        assert 0.0 <= meta["metrics"][key] <= 1.0


def test_log_file_is_written_next_to_the_model_and_is_not_a_json_file(trained):
    config, result = trained
    assert result.log_path == result.model_path.with_suffix(".log")
    text = result.log_path.read_text()
    assert "Starting training run" in text and "Fold 1" in text and "Model saved" in text
    assert sorted(p.suffix for p in Path(config.output_dir).iterdir()) == [".joblib", ".json", ".log"]


def test_two_runs_in_the_same_second_get_distinct_ids(tmp_path):
    config = make_config(tmp_path, n_splits=2, hyperparams={"n_estimators": 5})
    ids = {ModelTrainer().train(config).model_path.stem for _ in range(3)}
    assert len(ids) == 3


def test_id_is_claimed_atomically_even_if_the_exists_check_races(tmp_path, monkeypatch):
    """Two trainers can both pass an exists() check; exclusive creation must still keep them apart."""
    config = make_config(tmp_path, n_splits=2, hyperparams={"n_estimators": 5})
    first = ModelTrainer().train(config)
    before = first.model_path.read_bytes()
    real_exists = Path.exists
    out_dir = Path(config.output_dir)
    # The other process "saw" a free id: hide model files from the exists() check only.
    monkeypatch.setattr(
        Path, "exists", lambda self: False if self.parent == out_dir else real_exists(self)
    )
    second = ModelTrainer().train(config)
    monkeypatch.undo()
    assert second.model_path != first.model_path
    assert first.model_path.read_bytes() == before
    assert second.model_path.exists() and second.metadata_path.exists()


# ------------------------------------------------------------------
# Folds
# ------------------------------------------------------------------


def test_per_fold_records(trained):
    config, result = trained
    folds = strict_json(result.metadata_path)["cv_folds"]
    assert [f["fold"] for f in folds] == [1, 2, 3, 4]
    for f in folds:
        assert f["train_rows"] > 0 and f["test_rows"] > 0
        for key in ("accuracy", "f1", "precision", "recall", "baseline_accuracy"):
            assert 0.0 <= f[key] <= 1.0
        assert f["trades"] is not None and f["trades"] >= 0
        assert f["trade_return_pct"] is not None
    # Expanding window: every fold trains on at least as much as the one before.
    assert [f["train_rows"] for f in folds] == sorted(f["train_rows"] for f in folds)
    # Averages in metrics are the means of the folds.
    meta = strict_json(result.metadata_path)
    assert meta["metrics"]["accuracy"] == pytest.approx(np.mean([f["accuracy"] for f in folds]))


def test_baseline_is_the_majority_class_share_of_the_test_rows(tmp_path):
    config = make_config(tmp_path, n_splits=2, hyperparams={"n_estimators": 5})
    data = ModelTrainer()._build_features(ModelTrainer()._load_data(config), config)
    result = ModelTrainer().train(config)
    folds = strict_json(result.metadata_path)["cv_folds"]
    _, test_idx = list(ModelTrainer()._folds(data, config))[0]
    y = data.y[test_idx]
    assert folds[0]["baseline_accuracy"] == pytest.approx(max((y == 0).mean(), (y == 1).mean()))


@pytest.mark.parametrize("symbols", [("AAA",), ("AAA", "BBB", "CCC")])
@pytest.mark.parametrize("horizon", [1, 5, 12])
def test_embargo_keeps_train_labels_out_of_the_test_period(tmp_path, symbols, horizon):
    data_path = write_dataset(tmp_path / "e.csv", steps=150, symbols=symbols)
    config = make_config(tmp_path, data_path=data_path, horizon_bars=horizon, n_splits=5)
    trainer = ModelTrainer()
    data = trainer._build_features(trainer._load_data(config), config)
    folds = list(trainer._folds(data, config))
    assert len(folds) == 5
    for train_idx, test_idx in folds:
        test_first = data.step[test_idx].min()
        assert not (set(train_idx) & set(test_idx))
        # No training row sits within `horizon` time steps of the first test step: its label window
        # (horizon bars ahead) ends before the test period begins.
        assert data.step[train_idx].max() + horizon < test_first
        assert data.label_end[train_idx].max() < test_first
        # Folds are split by date, never by symbol: every symbol is present in each test fold.
        assert len(set(data.group[test_idx])) == len(symbols)
    # Test periods move forward and never overlap.
    firsts = [data.step[t].min() for _, t in folds]
    assert firsts == sorted(firsts)


def test_label_end_uses_each_symbols_own_bars(tmp_path):
    step = np.array([0, 1, 2, 3, 0, 2, 4, 6])  # symbol 0 has every day, symbol 1 every other day
    group = np.array([0, 0, 0, 0, 1, 1, 1, 1])
    end = ModelTrainer._label_end(step, group, horizon=2)
    assert end.tolist() == [2, 3, 2 + 2, 3 + 2, 4, 6, 4 + 2, 6 + 2]


def test_too_few_rows_for_the_embargo_is_a_clear_error(tmp_path):
    data = write_dataset(tmp_path / "tiny.csv", steps=12, symbols=("AAA",))
    config = make_config(tmp_path, data_path=data, horizon_bars=6, n_splits=5)
    with pytest.raises(ValueError, match="not enough data"):
        ModelTrainer().train(config)
    assert not Path(config.output_dir).exists() or not list(Path(config.output_dir).iterdir())


# ------------------------------------------------------------------
# Trading metrics
# ------------------------------------------------------------------


def test_trade_fold_uses_non_overlapping_rows_and_is_long_flat():
    preds = np.array([1, 1, 1, 1, -1, 0])
    ret = np.array([0.10, 0.50, 0.20, 0.50, -0.10, 0.30])
    step = np.arange(6)
    out = ModelTrainer._trade_fold(preds, ret, step, np.zeros(6, dtype=int), horizon=2)
    # Rows 0, 2 and 4 are used: long +10%, long +20%, then -1 = flat (no short on the -10% move).
    assert out["trades"] == 2 and out["wins"] == 2
    assert out["return_pct"] == pytest.approx((1.1 * 1.2 - 1) * 100)
    assert out["hit_rate"] == 1.0
    assert out["hold"] == pytest.approx([0.10, 0.20, -0.10])


def test_a_down_prediction_never_shorts():
    # Shorting a falling market would earn +20%; long/flat earns nothing.
    out = ModelTrainer._trade_fold(np.array([-1, -1]), np.array([-0.2, -0.2]), np.arange(2), np.zeros(2, dtype=int), horizon=1)
    assert out["return_pct"] == 0.0 and out["trades"] == 0


def test_trade_fold_flat_signals_earn_nothing_and_count_no_trades():
    out = ModelTrainer._trade_fold(np.zeros(4), np.array([0.1, 0.2, 0.3, 0.4]), np.arange(4), np.zeros(4, dtype=int), 1)
    assert out["trades"] == 0 and out["return_pct"] == 0.0 and out["hit_rate"] is None


def test_trade_fold_averages_symbols_in_the_same_slot():
    preds = np.array([1, 1])
    ret = np.array([0.10, -0.10])
    out = ModelTrainer._trade_fold(preds, ret, np.array([0, 0]), np.array([0, 1]), 1)
    assert out["strategy"] == pytest.approx([0.0]) and out["trades"] == 2 and out["wins"] == 1


def test_trade_fold_aligns_symbols_by_time_step_not_by_position():
    # Symbol 0 trades steps 0..9 at +1% each; symbol 1 only exists from step 5 and gains 10% then flat.
    steps0, steps1 = np.arange(10), np.arange(5, 10)
    step = np.concatenate([steps0, steps1])
    group = np.concatenate([np.zeros(10, dtype=int), np.ones(5, dtype=int)])
    ret = np.concatenate([np.full(10, 0.01), [0.10, 0, 0, 0, 0]])
    out = ModelTrainer._trade_fold(np.ones(15), ret, step, group, horizon=1)
    # A real equal-weight portfolio: 1% for steps 0-4, (1% + 10%) / 2 at step 5, then (1% + 0) / 2.
    assert out["hold"] == pytest.approx([0.01] * 5 + [0.055] + [0.005] * 4)
    assert out["strategy"] == pytest.approx(out["hold"])


def test_trade_fold_with_horizon_keeps_each_symbol_on_the_shared_grid():
    step = np.concatenate([np.arange(0, 8), np.arange(4, 8)])
    group = np.concatenate([np.zeros(8, dtype=int), np.ones(4, dtype=int)])
    ret = np.concatenate([np.arange(8) * 0.01, [0.5, 0, 0, 0]])
    out = ModelTrainer._trade_fold(np.ones(12), ret, step, group, horizon=4)
    # Grid buckets are steps 0-3 and 4-7. Symbol 1 only joins the second bucket, at its step 4.
    assert out["hold"] == pytest.approx([0.0, (0.04 + 0.5) / 2])


def test_trading_summary(trained):
    _, result = trained
    t = strict_json(result.metadata_path)["trading"]
    assert set(t) == {"strategy_return_pct", "buy_hold_return_pct", "hit_rate", "trades", "max_drawdown_pct", "sharpe"}
    assert t["max_drawdown_pct"] <= 0
    assert 0 <= t["hit_rate"] <= 1
    folds = strict_json(result.metadata_path)["cv_folds"]
    assert t["trades"] == sum(f["trades"] for f in folds)
    # The labels carry real signal, so trading them beats a coin flip.
    assert t["hit_rate"] > 0.5


def test_no_return_column_means_no_trading_numbers(tmp_path):
    config = make_config(tmp_path, return_column=None, n_splits=2, hyperparams={"n_estimators": 5})
    meta = strict_json(ModelTrainer().train(config).metadata_path)
    assert meta["trading"] is None
    assert all(f["trade_return_pct"] is None and f["trades"] is None for f in meta["cv_folds"])


def test_three_class_labels_trade_long_and_flat(tmp_path):
    data = write_dataset(tmp_path / "three.csv", kind="threshold", seed=5)
    config = make_config(tmp_path, data_path=data, n_splits=3, hyperparams={"n_estimators": 10})
    meta = strict_json(ModelTrainer().train(config).metadata_path)
    assert meta["trading"]["trades"] > 0


# ------------------------------------------------------------------
# Importances
# ------------------------------------------------------------------


@pytest.mark.parametrize("algorithm", ["random_forest", "gradient_boosting", "logistic_regression"])
def test_importances_sum_to_one(tmp_path, algorithm):
    config = make_config(tmp_path, algorithm=algorithm, hyperparams={}, n_splits=2)
    if algorithm != "logistic_regression":
        config.hyperparams = {"n_estimators": 10}
    meta = strict_json(ModelTrainer().train(config).metadata_path)
    imp = meta["feature_importance"]
    assert {i["feature"] for i in imp} == set(FEATURES)
    assert sum(i["importance"] for i in imp) == pytest.approx(1.0)
    assert [i["importance"] for i in imp] == sorted((i["importance"] for i in imp), reverse=True)
    # ret_1 drives the label, so it should be the top feature.
    assert imp[0]["feature"] == "ret_1"


def test_xgboost_handles_minus_one_labels_and_predicts_the_original_classes(tmp_path):
    pytest.importorskip("xgboost")
    data = write_dataset(tmp_path / "x.csv", kind="threshold", seed=9)
    config = make_config(tmp_path, algorithm="xgboost", data_path=data, n_splits=2, hyperparams={"n_estimators": 10, "max_depth": 3})
    result = ModelTrainer().train(config)
    import joblib

    preds = joblib.load(result.model_path).predict(np.array([[0.02, 50.0, 0.01], [-0.02, 50.0, 0.01]]))
    assert set(preds) <= {-1, 0, 1}
    meta = strict_json(result.metadata_path)
    assert "xgboost" in meta["library_versions"]
    assert sum(i["importance"] for i in meta["feature_importance"]) == pytest.approx(1.0)


# ------------------------------------------------------------------
# Hyperparameters
# ------------------------------------------------------------------


def test_valid_hyperparameters_are_normalised():
    assert validate_hyperparams("random_forest", {"n_estimators": 100.0, "max_depth": None, "max_features": 0.5}) == {
        "n_estimators": 100, "max_depth": None, "max_features": 0.5,
    }
    assert validate_hyperparams("gradient_boosting", {"learning_rate": 1, "subsample": 0.5})["learning_rate"] == 1.0
    assert validate_hyperparams("logistic_regression", {"C": 2, "max_iter": 50, "class_weight": "balanced"})
    assert validate_hyperparams("xgboost", {"colsample_bytree": 0.5, "random_state": 0})
    assert validate_hyperparams("random_forest", None) == {}


@pytest.mark.parametrize(
    "algorithm, params, fragment",
    [
        ("random_forest", {"learning_rate": 0.1}, "unknown hyperparameter 'learning_rate'"),
        ("random_forest", {"n_jobs": 4}, "n_jobs"),
        ("random_forest", {"n_estimators": 0}, "n_estimators"),
        ("random_forest", {"n_estimators": 5000}, "n_estimators"),
        ("random_forest", {"n_estimators": "100"}, "n_estimators"),
        ("random_forest", {"n_estimators": True}, "n_estimators"),
        ("random_forest", {"n_estimators": 10.5}, "n_estimators"),
        ("random_forest", {"max_features": "auto"}, "max_features"),
        ("random_forest", {"class_weight": "all"}, "class_weight"),
        ("random_forest", {"random_state": -3}, "random_state"),
        ("gradient_boosting", {"learning_rate": 0}, "learning_rate"),
        ("gradient_boosting", {"learning_rate": 2}, "learning_rate"),
        ("gradient_boosting", {"subsample": float("nan")}, "subsample"),
        ("logistic_regression", {"C": -1}, "'C'"),
        ("logistic_regression", {"random_state": 1}, "random_state"),
        ("xgboost", {"max_depth": 50}, "max_depth"),
        ("xgboost", {"min_samples_leaf": 2}, "min_samples_leaf"),
        ("svm", {}, "Unknown algorithm"),
    ],
)
def test_bad_hyperparameters_are_rejected_with_a_clear_message(algorithm, params, fragment):
    with pytest.raises(ValueError) as err:
        validate_hyperparams(algorithm, params)
    assert fragment in str(err.value)


def test_training_with_bad_hyperparameters_fails_before_writing_anything(tmp_path):
    config = make_config(tmp_path, hyperparams={"n_estimators": -5})
    with pytest.raises(ValueError, match="n_estimators"):
        ModelTrainer().train(config)
    assert not Path(config.output_dir).exists() or not list(Path(config.output_dir).iterdir())


# ------------------------------------------------------------------
# Data handling
# ------------------------------------------------------------------


def test_lags_do_not_cross_symbols(tmp_path):
    config = make_config(tmp_path, lag_periods=[1, 3])
    trainer = ModelTrainer()
    data = trainer._build_features(trainer._load_data(config), config)
    assert data.feature_names == FEATURES + [f"{f}_lag{n}" for f in FEATURES for n in (1, 3)]
    # 2 symbols x 240 rows, each symbol loses its first 3 rows to the longest lag.
    assert len(data.y) == 2 * (240 - 3)


def test_missing_columns_and_single_class_are_clear_errors(tmp_path):
    with pytest.raises(ValueError, match="nope"):
        ModelTrainer().train(make_config(tmp_path, feature_columns=["ret_1", "nope"]))
    path = tmp_path / "one.csv"
    path.write_text("time,symbol,a,fwd_ret,target\n" + "\n".join(f"2024-01-{d:02d},X,{d},0.1,1" for d in range(1, 29)) + "\n")
    with pytest.raises(ValueError, match="only one class"):
        ModelTrainer().train(make_config(tmp_path, data_path=path, feature_columns=["a"], horizon_bars=1))


def test_legacy_csv_without_time_still_trains(tmp_path):
    rng = np.random.default_rng(1)
    rows = ["a,b,signal"]
    for _ in range(200):
        a = rng.normal()
        rows.append(f"{a},{rng.normal()},{int(a > 0)}")
    path = tmp_path / "legacy.csv"
    path.write_text("\n".join(rows) + "\n")
    config = TrainingConfig(
        algorithm="gradient_boosting", data_path=str(path), target_column="signal", feature_columns=["a", "b"],
        hyperparams={"n_estimators": 10}, n_splits=3, output_dir=str(tmp_path / "m"),
    )
    result = ModelTrainer().train(config)
    meta = strict_json(result.metadata_path)
    assert meta["feature_names"][:2] == ["a", "b"] and "a_lag1" in meta["feature_names"]  # default lags kept
    assert meta["trading"] is None and meta["source"] == "user"


def test_unknown_config_keys_are_ignored_not_fatal():
    cfg = _config_from_json({"algorithm": "random_forest", "data_path": "x", "target_column": "t", "future_option": 1})
    assert cfg.algorithm == "random_forest"


# ------------------------------------------------------------------
# Predictor: batch equals single
# ------------------------------------------------------------------


def run_predict(*args: str) -> dict:
    out = subprocess.run(
        [sys.executable, "-m", "ml.predict", *args], cwd=APEX_PYTHON, capture_output=True, text=True, check=True
    )
    return json.loads(out.stdout.strip().splitlines()[-1])


@pytest.mark.parametrize("algorithm", ["random_forest", "logistic_regression"])
def test_batch_predict_equals_single_row_predict(tmp_path, algorithm):
    config = make_config(tmp_path, algorithm=algorithm, n_splits=2, hyperparams={} if algorithm == "logistic_regression" else {"n_estimators": 10})
    result = ModelTrainer().train(config)
    rng = np.random.default_rng(0)
    rows = [[float(rng.normal(0, 0.02)), float(rng.uniform(20, 80)), float(abs(rng.normal(0.01, 0.005)))] for _ in range(25)]

    batch = run_predict("--model-path", str(result.model_path), "--features-matrix-json", json.dumps(rows))
    assert len(batch["signals"]) == len(batch["probabilities"]) == 25
    assert batch["features_used"] == FEATURES
    for row, signal, prob in zip(rows, batch["signals"], batch["probabilities"]):
        single = run_predict("--model-path", str(result.model_path), "--features-json", json.dumps(dict(zip(FEATURES, row))))
        assert single["signal"] == signal
        assert single["probability"] == pytest.approx(prob)
        assert single["features_used"] == FEATURES


def test_batch_predict_rejects_wrong_width_and_accepts_empty(tmp_path):
    result = ModelTrainer().train(make_config(tmp_path, n_splits=2, hyperparams={"n_estimators": 5}))
    bad = subprocess.run(
        [sys.executable, "-m", "ml.predict", "--model-path", str(result.model_path), "--features-matrix-json", "[[1,2]]"],
        cwd=APEX_PYTHON, capture_output=True, text=True,
    )
    assert bad.returncode == 1 and "3 values" in bad.stdout
    empty = run_predict("--model-path", str(result.model_path), "--features-matrix-json", "[]")
    assert empty["signals"] == [] and empty["probabilities"] == []


def test_main_prints_one_json_line_and_reports_failures_on_stderr(tmp_path):
    config = make_config(tmp_path, n_splits=2, hyperparams={"n_estimators": 5})
    payload = json.dumps({**config.__dict__})
    ok = subprocess.run([sys.executable, "-m", "ml.trainer", "--config-json", payload], cwd=APEX_PYTHON, capture_output=True, text=True)
    assert ok.returncode == 0, ok.stderr
    result = json.loads(ok.stdout.strip().splitlines()[-1])
    assert result["model_id"].startswith("random_forest_") and result["duration_secs"] >= 0

    bad_payload = json.dumps({**config.__dict__, "hyperparams": {"bogus": 1}})
    bad = subprocess.run([sys.executable, "-m", "ml.trainer", "--config-json", bad_payload], cwd=APEX_PYTHON, capture_output=True, text=True)
    assert bad.returncode == 1
    assert bad.stderr.strip().splitlines()[-1].startswith("Training failed: unknown hyperparameter 'bogus'")
