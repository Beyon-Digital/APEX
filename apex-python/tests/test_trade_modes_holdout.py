"""Trade modes (long/flat vs long/short), the hold-out split, seeds and the new metadata."""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pytest

from ml.trainer import ModelTrainer, TrainingConfig
from tests.test_trainer import FEATURES, make_config, write_dataset

T = ModelTrainer


def strict(path: Path) -> dict:
    return json.loads(path.read_text())


# ------------------------------------------------------------------
# Class meaning
# ------------------------------------------------------------------


@pytest.mark.parametrize(
    "kind, mode, preds, expected",
    [
        ("direction", "long_flat", [1, 0, 1, 0], [1, 0, 1, 0]),
        ("direction", "long_short", [1, 0, 1, 0], [1, -1, 1, -1]),
        ("return_threshold", "long_flat", [1, 0, -1, 1], [1, 0, 0, 1]),
        ("return_threshold", "long_short", [1, 0, -1, 1], [1, 0, -1, 1]),
        # Without a label spec (0/1 data) 0 is flat in both modes: how automations read a legacy model.
        ("return_threshold", "long_short", [1, 0, 0], [1, 0, 0]),
        # Classes outside the mapping never hold a position.
        ("direction", "long_short", [2, -1], [0, 0]),
    ],
)
def test_positions_follow_the_label_kind_and_trade_mode(kind, mode, preds, expected):
    assert list(T._positions(np.array(preds), kind, mode)) == expected


def test_label_kind_comes_from_the_dataset_label_and_falls_back_to_the_classes():
    base = dict(algorithm="random_forest", data_path="x", target_column="t")
    direction = TrainingConfig(**base, dataset_meta={"label": {"kind": "direction"}})
    threshold = TrainingConfig(**base, dataset_meta={"label": {"kind": "return_threshold"}})
    legacy = TrainingConfig(**base)
    assert T._label_kind(direction, np.array([0, 1])) == "direction"
    assert T._label_kind(threshold, np.array([-1, 0, 1])) == "return_threshold"
    assert T._label_kind(legacy, np.array([-1, 0, 1])) == "return_threshold"
    assert T._label_kind(legacy, np.array([0, 1])) == "return_threshold"


# ------------------------------------------------------------------
# _trade_fold with shorting
# ------------------------------------------------------------------


def test_direction_labels_short_on_zero_in_long_short_mode_only():
    preds = np.array([1, 0, 0, 1])
    ret = np.array([0.10, -0.20, 0.05, -0.10])
    args = (preds, ret, np.arange(4), np.zeros(4, dtype=int), 1)
    flat = T._trade_fold(*args, kind="direction", trade_mode="long_flat")
    short = T._trade_fold(*args, kind="direction", trade_mode="long_short")
    assert flat["strategy"] == pytest.approx([0.10, 0.0, 0.0, -0.10])
    assert flat["trades"] == 2 and flat["wins"] == 1
    # Short earns -ret: +20% on the fall, -5% on the rise.
    assert short["strategy"] == pytest.approx([0.10, 0.20, -0.05, -0.10])
    assert short["trades"] == 4 and short["wins"] == 2
    assert short["return_pct"] == pytest.approx((1.10 * 1.20 * 0.95 * 0.90 - 1) * 100)
    assert short["hold"] == pytest.approx(flat["hold"])


def test_return_threshold_labels_short_on_minus_one_and_zero_stays_flat():
    preds = np.array([-1, 0, 1, -1])
    ret = np.array([-0.10, 0.50, 0.04, 0.02])
    args = (preds, ret, np.arange(4), np.zeros(4, dtype=int), 1)
    flat = T._trade_fold(*args, kind="return_threshold", trade_mode="long_flat")
    short = T._trade_fold(*args, kind="return_threshold", trade_mode="long_short")
    assert flat["strategy"] == pytest.approx([0.0, 0.0, 0.04, 0.0]) and flat["trades"] == 1
    assert short["strategy"] == pytest.approx([0.10, 0.0, 0.04, -0.02]) and short["trades"] == 3


def test_a_short_pays_the_round_trip_fee_instead_of_earning_it():
    # fwd_ret is already net of the fee (raw - fee). Shorting a raw -10% move with a 1% fee earns
    # 10% - 1% = 9%; naively negating the net return (-(-10% - 1%)... i.e. -r) would give 11%.
    fee = 0.01
    raw = -0.10
    r = raw - fee
    out = T._trade_fold(np.array([0]), np.array([r]), np.arange(1), np.zeros(1, dtype=int), 1, kind="direction", trade_mode="long_short", fee=fee)
    assert out["strategy"] == pytest.approx([0.09])
    # A long is untouched by the correction.
    out = T._trade_fold(np.array([1]), np.array([r]), np.arange(1), np.zeros(1, dtype=int), 1, kind="direction", trade_mode="long_short", fee=fee)
    assert out["strategy"] == pytest.approx([r])


# ------------------------------------------------------------------
# A model with a known answer
# ------------------------------------------------------------------


class Oracle:
    """Predicts the label from the first feature: +1 -> class 1, otherwise 0. Records every fit."""

    fits: list[np.ndarray] = []

    def fit(self, x, y):
        # column 1 is the row's time index; column 2, when present, is 1000 for the second symbol
        Oracle.fits.append(np.array(x[:, 1]) + (np.array(x[:, 2]) if x.shape[1] > 2 else 0))
        self.classes_ = np.unique(y)
        return self

    def predict(self, x):
        return (np.asarray(x)[:, 0] > 0).astype(int)


def oracle_dataset(path: Path, steps: int = 100, symbols=("AAA",), kind="direction") -> Path:
    """feature `sig` decides the label; `tix` is the time index. 1 -> +1%, 0 -> -2% next bar."""
    rows = ["time,symbol,sig,tix,fwd_ret,target"]
    for sym in symbols:
        for d in range(steps):
            up = (d * 7 + 3) % 5 < 2
            date = (np.datetime64("2024-01-01") + np.timedelta64(d, "D")).astype(str)
            fwd = 0.01 if up else -0.02
            rows.append(f"{date}T00:00:00Z,{sym},{1 if up else -1},{d},{fwd},{1 if up else 0}")
    path.write_text("\n".join(rows) + "\n")
    return path


@pytest.fixture()
def oracle(monkeypatch):
    Oracle.fits = []
    monkeypatch.setattr(T, "_model_factory", lambda self, config, hp, classes, est: (lambda: Oracle()))
    # The oracle cannot be pickled by reference from here; keep joblib simple.
    monkeypatch.setattr("ml.trainer.joblib.dump", lambda obj, handle: handle.write(b"x"))
    return Oracle


def oracle_config(tmp_path: Path, **overrides) -> TrainingConfig:
    data = oracle_dataset(tmp_path / "o.csv", steps=overrides.pop("steps", 100), symbols=overrides.pop("symbols", ("AAA",)))
    base = dict(
        data_path=str(data), feature_columns=["sig", "tix"], n_splits=3, hyperparams={}, horizon_bars=1,
        dataset_meta={"name": "o", "symbols": ["AAA"], "timeframe": "1d", "label": {"kind": "direction", "horizon_bars": 1}},
    )
    base.update(overrides)
    return make_config(tmp_path, **base)


def test_holdout_result_matches_a_manual_computation(tmp_path, oracle):
    config = oracle_config(tmp_path, holdout_pct=20, trade_mode="long_short")
    meta = strict(ModelTrainer().train(config).metadata_path)
    h = meta["holdout"]

    hold_days = range(80, 100)  # last 20% of 100 time steps
    ups = [(d * 7 + 3) % 5 < 2 for d in hold_days]
    assert h["rows"] == 20
    assert h["accuracy"] == 1.0  # the oracle is exact
    assert h["baseline_accuracy"] == pytest.approx(max(sum(ups), 20 - sum(ups)) / 20)
    # long_short: +1% on each up bar (long), +2% on each down bar (short of a -2% move).
    growth = np.prod([1.01 if u else 1.02 for u in ups])
    t = h["trading"]
    assert t["strategy_return_pct"] == pytest.approx((growth - 1) * 100)
    assert t["trades"] == 20 and t["hit_rate"] == 1.0
    assert t["buy_hold_return_pct"] == pytest.approx((np.prod([1.01 if u else 0.98 for u in ups]) - 1) * 100)
    assert meta["trade_mode"] == "long_short"

    # The same model scored long/flat only earns the long half.
    Oracle.fits = []
    flat = strict(ModelTrainer().train(oracle_config(tmp_path, holdout_pct=20, trade_mode="long_flat")).metadata_path)["holdout"]["trading"]
    assert flat["strategy_return_pct"] == pytest.approx((np.prod([1.01 if u else 1.0 for u in ups]) - 1) * 100)
    assert flat["trades"] == sum(ups)


def test_holdout_rows_never_reach_a_training_fold_or_the_final_fit_and_an_embargo_separates_them(tmp_path, oracle):
    horizon = 4
    config = oracle_config(
        tmp_path, holdout_pct=25, horizon_bars=horizon, symbols=("AAA", "BBB"), steps=120,
        dataset_meta={"name": "o", "symbols": ["AAA", "BBB"], "timeframe": "1d", "label": {"kind": "direction", "horizon_bars": horizon}},
    )
    meta = strict(ModelTrainer().train(config).metadata_path)
    hold_start = 120 - 30
    assert meta["holdout"]["rows"] == 30 * 2
    assert meta["holdout"]["start_step"] == hold_start and meta["holdout"]["embargo_steps"] == horizon
    assert len(Oracle.fits) == config.n_splits + 1  # one per fold, then the final fit
    for fit in Oracle.fits:
        assert fit.max() < hold_start - horizon, "a fit saw rows inside the embargo or the hold-out"
    final = Oracle.fits[-1]
    assert final.max() == hold_start - horizon - 1  # uses everything up to the embargo, nothing after
    for fit in Oracle.fits[:-1]:
        assert not np.isin(np.arange(hold_start - horizon, 120), fit).any()


def test_cross_validation_only_sees_the_region_before_the_holdout(tmp_path, oracle):
    config = oracle_config(tmp_path, holdout_pct=20, steps=100)
    with_hold = strict(ModelTrainer().train(config).metadata_path)
    without = strict(ModelTrainer().train(oracle_config(tmp_path, holdout_pct=0)).metadata_path)
    assert without["holdout"] is None
    assert sum(f["test_rows"] for f in with_hold["cv_folds"]) < sum(f["test_rows"] for f in without["cv_folds"])
    assert with_hold["rows"] == without["rows"] == 100  # `rows` counts every usable row


def test_label_windows_that_reach_into_the_holdout_are_dropped_from_the_fit(tmp_path, oracle):
    # Symbol BBB has bars on only every other day, so its horizon-3 label window spans six days.
    rows = ["time,symbol,sig,tix,sym,fwd_ret,target"]
    for d in range(100):
        day = f"{(np.datetime64('2024-01-01') + np.timedelta64(d, 'D')).astype(str)}T00:00:00Z"
        rows.append(f"{day},AAA,1,{d},0,0.01,{d % 2}")
        if d % 2 == 0:
            rows.append(f"{day},BBB,1,{d},1000,0.01,{(d // 2) % 2}")
    data = tmp_path / "gap.csv"
    data.write_text("\n".join(rows) + "\n")
    config = make_config(
        tmp_path, data_path=data, feature_columns=["sig", "tix", "sym"], n_splits=2, hyperparams={}, horizon_bars=3, holdout_pct=20,
        dataset_meta={"name": "g", "symbols": ["AAA", "BBB"], "timeframe": "1d", "label": {"kind": "direction", "horizon_bars": 3}},
    )
    ModelTrainer().train(config)
    # Hold-out starts at day 80; the embargo ends the fit region at day 76. BBB's label for day d is
    # decided on day d + 6, so its days 74 and 76 reach into the hold-out and must go. AAA's (d + 3) stay.
    final = Oracle.fits[-1]
    assert 76 in final and 72 + 1000 in final
    assert 74 + 1000 not in final and 76 + 1000 not in final
    assert final.max() <= 76 + 1000


def test_holdout_bounds_and_too_little_data(tmp_path):
    for bad in (-1, 41, 100):
        with pytest.raises(ValueError, match="holdout_pct"):
            ModelTrainer().train(make_config(tmp_path, holdout_pct=bad))
    with pytest.raises(ValueError, match="trade_mode"):
        ModelTrainer().train(make_config(tmp_path, trade_mode="short_only"))
    with pytest.raises(ValueError, match="too little"):
        ModelTrainer().train(make_config(tmp_path, data_path=write_dataset(tmp_path / "tiny.csv", steps=8, symbols=("AAA",)), holdout_pct=40, horizon_bars=5, n_splits=2))
    assert not (tmp_path / "models").exists() or not list((tmp_path / "models").iterdir())


def test_a_real_model_with_a_holdout_has_an_honest_second_score(tmp_path):
    config = make_config(tmp_path, holdout_pct=20, trade_mode="long_short", n_splits=3)
    result = ModelTrainer().train(config)
    meta = strict(result.metadata_path)
    h = meta["holdout"]
    assert set(h) >= {"rows", "accuracy", "baseline_accuracy", "trading"}
    assert 0 <= h["accuracy"] <= 1 and 0 < h["baseline_accuracy"] <= 1
    assert h["rows"] == 2 * 48  # 20% of 240 steps, two symbols
    assert h["trading"] is not None and h["trading"]["trades"] > 0
    assert meta["trade_mode"] == "long_short"
    # The saved model is the one scored on the hold-out: it was fit on fewer rows than exist.
    log = Path(result.log_path).read_text()
    assert "Hold-out (96 rows)" in log


def test_shorting_changes_the_aggregate_but_not_the_benchmark(tmp_path):
    flat = strict(ModelTrainer().train(make_config(tmp_path, trade_mode="long_flat")).metadata_path)
    short = strict(ModelTrainer().train(make_config(tmp_path, trade_mode="long_short")).metadata_path)
    assert flat["trade_mode"] == "long_flat" and short["trade_mode"] == "long_short"
    assert flat["trading"]["buy_hold_return_pct"] == pytest.approx(short["trading"]["buy_hold_return_pct"])
    assert short["trading"]["trades"] > flat["trading"]["trades"]  # class 0 now trades
    assert flat["metrics"] == short["metrics"]  # classification metrics do not depend on the trade mode


def test_threshold_labels_trade_all_three_classes_in_long_short(tmp_path):
    data = write_dataset(tmp_path / "three.csv", kind="threshold", seed=5)
    meta_label = {"horizon_bars": 5, "kind": "return_threshold", "threshold_pct": 0.4}
    ds = {"name": "ds", "symbols": ["AAA", "BBB"], "timeframe": "1d", "label": meta_label}
    flat = strict(ModelTrainer().train(make_config(tmp_path, data_path=data, n_splits=3, dataset_meta=ds, trade_mode="long_flat")).metadata_path)
    short = strict(ModelTrainer().train(make_config(tmp_path, data_path=data, n_splits=3, dataset_meta=ds, trade_mode="long_short")).metadata_path)
    assert short["trading"]["trades"] > flat["trading"]["trades"]


# ------------------------------------------------------------------
# Seed, metadata
# ------------------------------------------------------------------


def test_random_state_defaults_to_a_fixed_seed_and_is_recorded(tmp_path):
    a = strict(ModelTrainer().train(make_config(tmp_path, n_splits=2)).metadata_path)
    b = strict(ModelTrainer().train(make_config(tmp_path, n_splits=2)).metadata_path)
    assert a["random_state"] == 42 and a["hyperparams"]["random_state"] == 42
    assert a["metrics"] == b["metrics"]
    c = strict(ModelTrainer().train(make_config(tmp_path, n_splits=2, random_state=7)).metadata_path)
    assert c["random_state"] == 7 and c["hyperparams"]["random_state"] == 7
    for bad in (-1, 2**31, True, 1.5):
        with pytest.raises(ValueError, match="random_state"):
            ModelTrainer().train(make_config(tmp_path, n_splits=2, random_state=bad))


def test_metadata_carries_the_dataset_recipe_for_inference(tmp_path):
    recipe = [{"name": "mom", "expr": "close / shift(close, 5) - 1"}]
    ds = {
        "name": "ds", "symbols": ["AAA", "BBB"], "timeframe": "1d", "fee_pct": 0.1,
        "label": {"horizon_bars": 5, "kind": "direction", "threshold_pct": 0.0},
        "custom_features": recipe, "context_symbols": ["SPY"],
    }
    meta = strict(ModelTrainer().train(make_config(tmp_path, n_splits=2, dataset_meta=ds)).metadata_path)
    assert meta["custom_features"] == recipe and meta["context_symbols"] == ["SPY"] and meta["fee_pct"] == 0.1
    assert meta["trade_mode"] == "long_flat" and meta["holdout"] is None and meta["estimator_file"] is None
    # Everything an older reader needed is still there.
    for key in ("model_id", "algorithm", "hyperparams", "feature_names", "target_column", "metrics", "model_file", "created_utc"):
        assert key in meta
    plain = strict(ModelTrainer().train(make_config(tmp_path, n_splits=2)).metadata_path)
    assert plain["custom_features"] == [] and plain["context_symbols"] == [] and plain["fee_pct"] == 0.0


def test_unknown_and_new_config_keys_from_json_are_understood():
    from ml.trainer import _config_from_json

    cfg = _config_from_json({
        "algorithm": "random_forest", "data_path": "x", "target_column": "t",
        "trade_mode": "long_short", "holdout_pct": 15, "random_state": 3, "estimator_file": "a.py", "future_option": 1,
    })
    assert (cfg.trade_mode, cfg.holdout_pct, cfg.random_state, cfg.estimator_file) == ("long_short", 15, 3, "a.py")
