"""Tests for algorithm recipes: validation, steps, voting, time-aware stacking and calibration,
trainer integration, scoring through ``python -m ml.predict`` and the command line."""

from __future__ import annotations

import json
import pickle
import re
import subprocess
import sys
from pathlib import Path

import joblib
import numpy as np
import pytest
from sklearn.base import BaseEstimator, ClassifierMixin
from sklearn.ensemble import RandomForestClassifier
from sklearn.pipeline import Pipeline
from sklearn.tree import DecisionTreeClassifier

from ml import recipes
from ml.estimators import (
    ClipOutliers,
    KBestSelector,
    PCAReducer,
    TimeAwareCalibrated,
    TimeAwareStacker,
)
from ml.trainer import ModelTrainer, TrainingConfig
from tests.test_trainer import FEATURES, make_config, write_dataset

APEX_PYTHON = Path(__file__).resolve().parents[1]
REPO = APEX_PYTHON.parent


# ------------------------------------------------------------------
# Helpers
# ------------------------------------------------------------------


def rf(n: int = 10, depth: int = 4) -> dict:
    return {"algorithm": "random_forest", "params": {"n_estimators": n, "max_depth": depth}}


def recipe(model: dict | None = None, steps: list | None = None, calibrate: str = "none", **extra) -> dict:
    return {
        "name": "my_algo", "label": "My algorithm", "description": "", "steps": steps if steps is not None else [],
        "model": model or {"kind": "single", **rf()}, "calibrate": calibrate, **extra,
    }


def frame(n: int = 400, d: int = 6, classes: int = 3, seed: int = 0):
    """Features where column 0 decides the label (plus noise), in 'time' order."""
    rng = np.random.default_rng(seed)
    x = rng.normal(size=(n, d))
    score = x[:, 0] + rng.normal(0, 0.6, n)
    if classes == 2:
        y = (score > 0).astype(int)
    else:
        y = np.where(score > 0.5, 1, np.where(score < -0.5, -1, 0))
    return x, y


class Recorder(DecisionTreeClassifier):
    """A tree that remembers which rows it was fitted on (column 0 of X holds the row number)."""

    fits: list = []

    def fit(self, x, y, **kw):
        type(self).fits.append((int(np.min(x[:, 0])), int(np.max(x[:, 0])), len(x)))
        return super().fit(x, y, **kw)


class VoteOnly(ClassifierMixin, BaseEstimator):
    """Nearest class mean on column 1; has predict() but no predict_proba()."""

    def fit(self, x, y):
        self.classes_ = np.unique(y)
        self.means_ = np.array([x[y == c, 1].mean() for c in self.classes_])
        return self

    def predict(self, x):
        return self.classes_[np.abs(x[:, [1]] - self.means_).argmin(axis=1)]


# ------------------------------------------------------------------
# Validation
# ------------------------------------------------------------------


def test_valid_recipe_reports_counts_size_and_no_errors():
    r = recipe(
        {"kind": "stacking", "members": [rf(), {"algorithm": "extra_trees", "params": {}}], "final": {"algorithm": "logistic_regression", "params": {}}, "blocks": 4},
        steps=[{"kind": "standard_scaler", "params": {}}, {"kind": "select_k_best", "params": {"k": 3}}],
    )
    check = recipes.validate(r)
    assert check["ok"] and check["errors"] == []
    assert check["steps"] == 2 and check["models"] == 3
    assert isinstance(check["est_model_mb"], float) and check["est_model_mb"] > 0


def test_validation_collects_every_issue_with_a_path():
    r = {
        "name": "Bad Name", "label": " ",
        "steps": [
            {"kind": "select_k_best", "params": {"k": 0, "score": "nope", "extra": 1}},
            {"kind": "no_such_step", "params": {}},
            {"kind": "clip_outliers", "params": {"lower_pct": 10, "upper_pct": 5}},
            {"kind": "pca", "params": {"n_components": 2.5}},
        ],
        "model": {
            "kind": "voting", "voting": "maybe",
            "members": [
                {"algorithm": "random_forest", "params": {"n_estimators": 10}},
                {"algorithm": "gradient_boosting", "params": {"learning_rate": "fast"}},
                {"algorithm": "random_forest", "params": {"n_estimators": 0, "bogus": 1}, "weight": -1},
                {"algorithm": "custom", "params": {}},
            ],
        },
        "calibrate": "magic",
    }
    check = recipes.validate(r)
    assert not check["ok"]
    paths = {e["path"] for e in check["errors"]}
    expected = {
        "name", "label", "steps[0].params.k", "steps[0].params.score", "steps[0].params.extra", "steps[1].kind",
        "steps[2].params.upper_pct", "steps[3].params.n_components", "calibrate", "model.voting",
        "model.members[1].params.learning_rate", "model.members[2].params.n_estimators", "model.members[2].params.bogus",
        "model.members[2].weight", "model.members[3].algorithm",
    }
    assert expected <= paths, expected - paths
    by_path = {e["path"]: e["message"] for e in check["errors"]}
    # the catalog's own message is reported for member parameters
    assert "n_estimators" in by_path["model.members[2].params.n_estimators"] and "random_forest" in by_path["model.members[2].params.n_estimators"]
    assert "unknown hyperparameter 'bogus'" in by_path["model.members[2].params.bogus"]


@pytest.mark.parametrize("bad, path", [
    ({"kind": "voting", "voting": "soft", "members": [rf()]}, "model.members"),
    ({"kind": "voting", "voting": "soft", "members": [rf()] * 7}, "model.members"),
    ({"kind": "stacking", "members": [rf(), rf()], "final": rf(), "blocks": 2}, "model.blocks"),
    ({"kind": "stacking", "members": [rf(), rf()], "final": rf(), "blocks": 9}, "model.blocks"),
    ({"kind": "stacking", "members": [rf(), rf()], "final": {"algorithm": "recipe", "params": {}}, "blocks": 4}, "model.final.algorithm"),
    ({"kind": "mystery"}, "model.kind"),
    ({"kind": "single", "algorithm": "custom", "params": {}}, "model.algorithm"),
])
def test_model_limits_and_algorithms(bad, path):
    check = recipes.validate(recipe(bad))
    assert not check["ok"] and path in {e["path"] for e in check["errors"]}


def test_too_many_steps_and_garbage_input_do_not_crash():
    check = recipes.validate(recipe(steps=[{"kind": "robust_scaler", "params": {}}] * 9))
    assert "steps" in {e["path"] for e in check["errors"]}
    for junk in (None, 5, "x", [], {"model": 3, "steps": "no"}):
        out = recipes.validate(junk)
        assert out["ok"] is False and out["errors"]


def test_warnings():
    pca = recipes.validate(recipe(steps=[{"kind": "pca", "params": {}}]))
    assert pca["ok"] and any("PCA" in w for w in pca["warnings"])
    hard = recipes.validate(recipe({"kind": "voting", "voting": "hard", "members": [rf(), rf()]}, calibrate="sigmoid"))
    assert hard["ok"] and any("calibration is ignored" in w for w in hard["warnings"])
    logit = recipes.validate(recipe({"kind": "single", "algorithm": "logistic_regression", "params": {}}))
    assert any("scaler" in w for w in logit["warnings"])
    huge = recipes.validate(recipe({"kind": "single", "algorithm": "random_forest", "params": {"n_estimators": 5000}}))
    assert huge["ok"] and huge["est_model_mb"] > recipes.SIZE_WARN_MB and any("MB" in w for w in huge["warnings"])


def test_blocks_lists_every_step_and_the_installed_members():
    b = recipes.blocks()
    assert [s["kind"] for s in b["steps"]] == [
        "standard_scaler", "robust_scaler", "minmax_scaler", "quantile_transformer", "power_transformer",
        "clip_outliers", "variance_threshold", "select_k_best", "pca",
    ]
    ids = {m["id"] for m in b["members"]}
    assert {"random_forest", "extra_trees", "gradient_boosting", "hist_gradient_boosting", "logistic_regression"} <= ids
    assert "custom" not in ids and "recipe" not in ids
    assert b["limits"] == {"max_steps": 8, "max_members": 6, "min_members": 2, "stacking_blocks": [3, 8]}
    # every default in the table passes the validator it documents
    for s in b["steps"]:
        check = recipes.validate(recipe(steps=[{"kind": s["kind"], "params": {p["key"]: p["default"] for p in s["params"]}}]))
        assert check["ok"], (s["kind"], check["errors"])


# ------------------------------------------------------------------
# Steps
# ------------------------------------------------------------------


@pytest.mark.parametrize("kind", [s["kind"] for s in recipes.blocks()["steps"]])
def test_every_step_kind_builds_fits_and_predicts(kind):
    x, y = frame()
    x[:, 5] = 1.0  # a flat column for variance_threshold to drop
    model = recipes.build(recipe(steps=[{"kind": kind, "params": {}}]))
    model.fit(x[:300], y[:300])
    pred = model.predict(x[300:])
    assert pred.shape == (100,) and set(pred) <= {-1, 0, 1}
    assert model.predict_proba(x[300:]).shape == (100, 3)


def test_step_parameters_reach_the_transformer():
    x, y = frame()
    m = recipes.build(recipe(steps=[{"kind": "standard_scaler", "params": {"with_mean": False}},
                                    {"kind": "quantile_transformer", "params": {"output_distribution": "normal", "n_quantiles": 50}}]))
    m.fit(x, y)
    assert m.steps[0][1].with_mean is False
    assert m.steps[1][1].output_distribution == "normal" and m.steps[1][1].n_quantiles == 50


def test_clip_outliers_learns_its_bounds_from_the_training_rows_only():
    rng = np.random.default_rng(1)
    train = rng.normal(size=(300, 3))
    test = rng.normal(size=(50, 3))
    test[7, 1] = 1e6  # an outlier that only exists in the test rows
    clip = ClipOutliers(1, 99).fit(train)
    assert clip.upper_.max() < 10
    out = clip.transform(test)
    assert out[7, 1] == clip.upper_[1]
    assert (out >= clip.lower_).all() and (out <= clip.upper_).all()
    # fitting inside a pipeline on the earlier rows behaves the same, and live data cannot move the bounds
    pipe = Pipeline([("clip", ClipOutliers(1, 99)), ("m", RandomForestClassifier(5, random_state=0))])
    x = np.vstack([train, test])
    y = (x[:, 0] > 0).astype(int)
    pipe.fit(x[:300], y[:300])
    before = pipe.named_steps["clip"].upper_.copy()
    pipe.predict(test)
    assert np.array_equal(before, pipe.named_steps["clip"].upper_)
    assert before[1] < 10


def test_select_k_best_is_capped_to_the_feature_count():
    x, y = frame(d=4)
    for score in ("f_classif", "mutual_info_classif"):
        sel = KBestSelector(k=50, score=score, random_state=0).fit(x, y)
        assert sel.transform(x).shape[1] == 4 and sel.get_support().all()
    sel = KBestSelector(k=2).fit(x, y)
    assert sel.transform(x).shape[1] == 2 and sel.get_support()[0]  # column 0 carries the label
    # through a recipe: k larger than the number of features does not fail
    m = recipes.build(recipe(steps=[{"kind": "select_k_best", "params": {"k": 400}}]))
    m.fit(x, y)
    assert m.predict(x).shape == y.shape


def test_pca_variance_share_and_component_count():
    rng = np.random.default_rng(2)
    base = rng.normal(size=(500, 2))
    x = np.hstack([base @ rng.normal(size=(2, 4)) + rng.normal(0, 0.01, (500, 4)), rng.normal(0, 0.01, (500, 2))])
    share = PCAReducer(0.95).fit(x)
    assert share.n_components_ == 2
    assert share.explained_variance_ratio_.sum() >= 0.95
    assert PCAReducer(0.999999).fit(x).n_components_ > share.n_components_
    assert PCAReducer(3).fit(x).transform(x).shape == (500, 3)
    assert PCAReducer(50).fit(x).transform(x).shape == (500, 6)  # capped to what the data allows
    white = PCAReducer(2, whiten=True).fit(x).transform(x)
    assert np.allclose(white.std(axis=0, ddof=1), 1.0, atol=1e-6)


# ------------------------------------------------------------------
# Voting
# ------------------------------------------------------------------


def test_soft_voting_averages_probabilities_by_weight():
    x, y = frame()
    model = recipes.build(recipe({"kind": "voting", "voting": "soft", "members": [
        {**rf(), "weight": 3}, {"algorithm": "logistic_regression", "params": {}, "weight": 1}]}, steps=[{"kind": "standard_scaler", "params": {}}]))
    model.fit(x, y)
    vote = model.named_steps["model"]
    assert list(vote.weights) == [3.0, 1.0] and vote.voting == "soft"
    scaled = model.named_steps["s0_standard_scaler"].transform(x)
    p_rf, p_lr = (e.predict_proba(scaled) for e in vote.estimators_)
    assert np.allclose(model.predict_proba(x), (3 * p_rf + p_lr) / 4)
    assert np.array_equal(model.predict(x), vote.classes_[np.argmax(model.predict_proba(x), axis=1)])


def test_hard_voting_has_no_probabilities_and_weights_decide():
    x, y = frame()
    members = [{**rf(5, 2), "weight": 1}, {"algorithm": "extra_trees", "params": {"n_estimators": 5, "max_depth": 2}, "weight": 1},
               {"algorithm": "random_forest", "params": {"n_estimators": 20, "max_depth": 6}, "weight": 5}]
    model = recipes.build(recipe({"kind": "voting", "voting": "hard", "members": members}, calibrate="sigmoid"))
    assert isinstance(model, Pipeline)  # no calibrator around hard voting
    model.fit(x, y)
    assert not hasattr(model, "predict_proba")
    vote = model.named_steps["model"]
    heavy = vote.classes_[vote.estimators_[2].predict(x)]  # the members see labels encoded as 0..k-1
    assert np.array_equal(model.predict(x), heavy)  # weight 5 outvotes 1 + 1


def test_xgboost_member_handles_minus_one_labels():
    x, y = frame()
    model = recipes.build(recipe({"kind": "voting", "voting": "soft", "members": [
        {"algorithm": "xgboost", "params": {"n_estimators": 10}}, rf()]}))
    model.fit(x, y)
    assert set(model.predict(x)) <= {-1, 0, 1} and model.predict_proba(x).shape == (400, 3)


# ------------------------------------------------------------------
# Stacking
# ------------------------------------------------------------------


def memorising_stacker(embargo: int = 10, blocks: int = 4, **kw) -> TimeAwareStacker:
    members = [("tree", DecisionTreeClassifier(random_state=0)), ("tree2", DecisionTreeClassifier(max_features=1, random_state=1))]
    return TimeAwareStacker(members, DecisionTreeClassifier(max_depth=3, random_state=0), blocks=blocks, embargo_rows=embargo, keep_oof=True, **kw)


def random_rows(n: int = 300, seed: int = 4):
    rng = np.random.default_rng(seed)
    return rng.normal(size=(n, 3)), rng.integers(0, 2, n)  # no signal at all


def block_slice(stacker: TimeAwareStacker, j: int) -> slice:
    b = stacker.block_bounds_
    return slice(int(b[j] - b[1]), int(b[j + 1] - b[1]))


def test_stacker_out_of_fold_predictions_never_see_the_label_of_their_block_or_later():
    x, y = random_rows()
    base = memorising_stacker().fit(x, y)
    b, e = base.block_bounds_, 10
    assert list(b) == [0, 60, 120, 180, 240, 300]

    # Future rows would score perfectly: a tree fitted on everything memorises the noise ...
    assert (DecisionTreeClassifier(random_state=0).fit(x, y).predict(x) == y).all()
    # ... but the stacker's own out-of-fold votes of that same tree are no better than chance.
    oof_vote = base.oof_[:, :2].argmax(axis=1)  # member 0's class-1 probability vs class 0
    assert abs((oof_vote == y[b[1]:]).mean() - 0.5) < 0.12

    for j in (2, 3):
        y2 = y.copy()
        rng = np.random.default_rng(99)
        y2[b[j] - e:] = rng.permutation(1 - y[b[j] - e:])  # every label that block j may not see (and later ones)
        other = memorising_stacker().fit(x, y2)
        for earlier in range(1, j + 1):
            assert np.array_equal(base.oof_[block_slice(base, earlier)], other.oof_[block_slice(other, earlier)]), (j, earlier)
        assert not np.array_equal(base.oof_[block_slice(base, j + 1)], other.oof_[block_slice(other, j + 1)])

    # The embargo is exact: changing the last label that IS allowed into training does change block 2.
    y3 = y.copy()
    y3[b[2] - e - 1] = 1 - y3[b[2] - e - 1]
    y3[b[2] - e - 20:b[2] - e] = 1 - y3[b[2] - e - 20:b[2] - e]
    changed = memorising_stacker().fit(x, y3)
    assert not np.array_equal(base.oof_[block_slice(base, 2)], changed.oof_[block_slice(changed, 2)])


def test_stacker_fits_members_on_exactly_the_allowed_rows():
    n, e = 300, 10
    x = np.column_stack([np.arange(n), np.random.default_rng(0).normal(size=(n, 2))])
    y = np.random.default_rng(1).integers(0, 2, n)
    Recorder.fits = []
    stacker = TimeAwareStacker([("a", Recorder(random_state=0))], DecisionTreeClassifier(max_depth=2), blocks=4, embargo_rows=e).fit(x, y)
    b = stacker.block_bounds_
    out_of_fold, refit = Recorder.fits[:4], Recorder.fits[4:]
    for j, (lo, hi, count) in enumerate(out_of_fold, start=1):
        assert lo == 0 and hi == b[j] - e - 1 and count == b[j] - e  # all earlier rows, minus the embargo, nothing later
    assert refit == [(0, n - 1, n)]  # the prediction-time members use every training row


def test_stacker_block_missing_a_class_and_single_class_prefix_do_not_crash():
    x, _ = random_rows(400)
    y = np.zeros(400, dtype=int)
    y[100:] = np.random.default_rng(5).integers(0, 2, 300)
    y[330:] = np.random.default_rng(6).integers(0, 3, 70)  # class 2 only in the last rows
    stacker = TimeAwareStacker([("rf", RandomForestClassifier(10, random_state=0)), ("tree", DecisionTreeClassifier(random_state=0))],
                               RandomForestClassifier(10, random_state=0), blocks=4, embargo_rows=5, keep_oof=True).fit(x, y)
    # block 1's members were trained on rows 0..74: a single class
    assert list(stacker.classes_) == [0, 1, 2]
    proba = stacker.predict_proba(x)
    assert proba.shape == (400, 3) and np.allclose(proba.sum(axis=1), 1) and np.isfinite(proba).all()
    first = stacker.oof_[block_slice(stacker, 1)]
    assert np.allclose(first[:, :3], np.tile([1.0, 0.0, 0.0], (len(first), 1)))  # the constant member says class 0
    assert (stacker.oof_[:, [2, 5]] == 0).all()  # no member had seen class 2 before the last block
    assert set(stacker.predict(x)) <= {0, 1, 2}


def test_stacker_member_without_predict_proba_contributes_votes():
    x, y = random_rows()
    stacker = TimeAwareStacker([("vote", VoteOnly()), ("tree", DecisionTreeClassifier(max_depth=3, random_state=0))],
                               DecisionTreeClassifier(max_depth=3, random_state=0), blocks=3, embargo_rows=0, keep_oof=True).fit(x, y)
    votes = stacker.oof_[:, :2]
    assert set(np.unique(votes)) <= {0.0, 1.0} and (votes.sum(axis=1) == 1).all()
    assert stacker.predict_proba(x).shape == (300, 2)


@pytest.mark.parametrize("labels", [(0, 1), (-1, 0, 1)])
def test_stacker_supports_the_apps_label_sets(labels):
    x, y = frame(classes=len(labels))
    stacker = TimeAwareStacker([("rf", RandomForestClassifier(10, max_depth=4, random_state=0))], RandomForestClassifier(10, max_depth=3, random_state=0),
                               blocks=3, embargo_rows=4).fit(x, y)
    assert list(stacker.classes_) == list(labels)
    assert set(stacker.predict(x)) <= set(labels) and stacker.predict_proba(x).shape == (400, len(labels))
    clone = pickle.loads(pickle.dumps(stacker))
    assert np.array_equal(clone.predict(x), stacker.predict(x))


def test_stacker_final_estimator_without_proba_still_predicts():
    x, y = random_rows()
    stacker = TimeAwareStacker([("tree", DecisionTreeClassifier(random_state=0))], VoteOnly(), blocks=3).fit(x, y)
    assert not hasattr(stacker, "predict_proba")
    assert stacker.predict(x).shape == (300,)


def test_stacker_refuses_too_few_rows_with_a_clear_message():
    x, y = random_rows(40)
    with pytest.raises(ValueError, match="not enough rows for stacking"):
        memorising_stacker(embargo=5, blocks=5).fit(x, y)
    x, y = random_rows(300)
    with pytest.raises(ValueError, match="labels reaching 90 rows ahead"):
        memorising_stacker(embargo=90, blocks=4).fit(x, y)
    with pytest.raises(ValueError, match="at least two classes"):
        memorising_stacker().fit(x, np.zeros(300, dtype=int))


def test_recipe_stacking_builds_with_the_embargo_and_blocks():
    model = recipes.build(recipe({"kind": "stacking", "members": [rf(), rf()], "final": {"algorithm": "logistic_regression", "params": {}}, "blocks": 5}),
                          embargo_rows=12)
    stacker = stacker_of(model)
    assert isinstance(stacker, TimeAwareStacker) and stacker.blocks == 5 and stacker.embargo_rows == 12
    x, y = frame(600)
    model.fit(x, y)
    assert model.predict_proba(x).shape == (600, 3)


def stacker_of(model) -> TimeAwareStacker:
    return model if isinstance(model, TimeAwareStacker) else model.named_steps["model"]


def test_recipe_steps_are_fitted_inside_each_stacking_block_so_nothing_leaks():
    """A label-using step (select_k_best) and a scaler must only see the rows a block's members may see."""
    rng = np.random.default_rng(3)
    n, e = 400, 5
    x = rng.normal(size=(n, 40))
    y = rng.integers(0, 2, n)  # pure noise: any dependence on later labels is a leak
    steps = [{"kind": "standard_scaler", "params": {}}, {"kind": "select_k_best", "params": {"k": 4}}]
    stacking = {"kind": "stacking", "blocks": 4, "members": [{"algorithm": "logistic_regression", "params": {}}, rf(5, 3)],
                "final": {"algorithm": "logistic_regression", "params": {}}}

    def oof(xx, yy):
        model = recipes.build(recipe(stacking, steps=steps), embargo_rows=e)
        stacker_of(model).set_params(keep_oof=True)
        model.fit(xx, yy)
        stacker = stacker_of(model)
        return stacker.oof_, stacker.block_bounds_

    base, b = oof(x, y)
    for j in (2, 3):
        later = slice(b[j] - e, n)
        y2 = y.copy()
        y2[later] = 1 - y2[later]  # every label block j may not use, and all later ones
        x2 = x.copy()
        x2[b[j + 1]:] = rng.normal(5, 3, size=x2[b[j + 1]:].shape)  # features of the blocks after block j
        other, _ = oof(x2, y2)
        upto = slice(0, int(b[j + 1] - b[1]))  # the meta rows of blocks 1..j
        assert np.array_equal(base[upto], other[upto]), j


# ------------------------------------------------------------------
# Calibration
# ------------------------------------------------------------------


@pytest.mark.parametrize("method", ["sigmoid", "isotonic"])
def test_calibrator_uses_rows_later_than_the_models_with_the_embargo(method):
    n, e = 400, 12
    x = np.column_stack([np.arange(n), np.random.default_rng(0).normal(size=(n, 2))])
    y = (x[:, 1] + np.random.default_rng(1).normal(0, 1, n) > 0).astype(int)
    Recorder.fits = []
    cal = TimeAwareCalibrated(Recorder(random_state=0), method=method, embargo_rows=e).fit(x, y)
    assert cal.calibration_start_ == n - 80 and cal.model_rows_ == n - 80 - e
    assert cal.calibration_start_ - cal.model_rows_ == e
    assert Recorder.fits == [(0, cal.model_rows_ - 1, cal.model_rows_)]  # the model never saw the calibration slice or the gap


@pytest.mark.parametrize("method", ["sigmoid", "isotonic"])
def test_calibration_reduces_overconfidence(method):
    x, y = frame(1500, classes=2, seed=11)
    cal = TimeAwareCalibrated(DecisionTreeClassifier(random_state=0), method=method, embargo_rows=5).fit(x[:1000], y[:1000])
    raw = cal.estimator_.predict_proba(x[1000:])
    out = cal.predict_proba(x[1000:])
    assert np.allclose(out.sum(axis=1), 1) and np.isfinite(out).all() and out.shape == raw.shape
    later = y[1000:]

    def gap(p):
        return abs(p.max(axis=1).mean() - (cal.classes_[p.argmax(axis=1)] == later).mean())

    assert gap(raw) > 0.15 and gap(out) < gap(raw) / 2


def test_calibrator_with_a_single_class_slice_and_too_few_rows():
    x, y = frame(300, classes=2)
    y[-60:] = 1  # the calibration slice holds a single class: that class keeps its raw probability
    cal = TimeAwareCalibrated(RandomForestClassifier(10, random_state=0), embargo_rows=3).fit(x, y)
    assert cal.calibrators_ == [None, None] and np.allclose(cal.predict_proba(x).sum(axis=1), 1)
    with pytest.raises(ValueError, match="not enough rows to calibrate"):
        TimeAwareCalibrated(RandomForestClassifier(5), embargo_rows=3).fit(x[:50], y[:50])
    with pytest.raises(ValueError, match="sigmoid"):
        TimeAwareCalibrated(RandomForestClassifier(5), method="platt").fit(x, y)


def test_recipe_calibration_wraps_the_whole_pipeline():
    r = recipe(steps=[{"kind": "clip_outliers", "params": {}}], calibrate="isotonic")
    model = recipes.build(r, embargo_rows=7)
    assert isinstance(model, TimeAwareCalibrated) and model.method == "isotonic" and model.embargo_rows == 7
    assert isinstance(model.estimator, Pipeline)  # the steps are fitted on the model's rows only
    x, y = frame(600)
    model.fit(x, y)
    assert model.predict_proba(x).shape == (600, 3)
    assert model.estimator_.named_steps["s0_clip_outliers"].lower_.shape == (6,)


# ------------------------------------------------------------------
# Trainer
# ------------------------------------------------------------------

VOTING = {"kind": "voting", "voting": "soft", "members": [rf(15), {"algorithm": "extra_trees", "params": {"n_estimators": 15, "max_depth": 4}},
                                                          {"algorithm": "logistic_regression", "params": {}, "weight": 2}]}
STACKING = {"kind": "stacking", "blocks": 3, "members": [rf(15), {"algorithm": "xgboost", "params": {"n_estimators": 10}}],
            "final": {"algorithm": "logistic_regression", "params": {}}}
STEPS = [{"kind": "clip_outliers", "params": {}}, {"kind": "standard_scaler", "params": {}}]


@pytest.fixture(scope="module")
def dataset(tmp_path_factory):
    return write_dataset(tmp_path_factory.mktemp("data") / "ds.csv", steps=900, kind="return_threshold")


def train(tmp_path, dataset, rec, **overrides):
    config = make_config(tmp_path, data_path=dataset, algorithm="recipe", hyperparams={}, recipe=rec, recipe_name=rec.get("name"),
                         holdout_pct=15, **overrides)
    return ModelTrainer().train(config)


@pytest.mark.parametrize("model, calibrate", [
    ({"kind": "single", **rf()}, "none"),
    (VOTING, "sigmoid"),
    (STACKING, "none"),
    (STACKING, "isotonic"),
])
def test_trainer_runs_single_voting_and_stacking_recipes(tmp_path, dataset, model, calibrate):
    rec = recipe(model, steps=STEPS, calibrate=calibrate)
    result = train(tmp_path, dataset, rec)
    meta = json.loads(result.metadata_path.read_text())
    assert meta["algorithm"] == "recipe" and meta["model_file"].startswith("recipe_")
    assert meta["recipe_name"] == "my_algo" and meta["hyperparams"] == {} and meta["random_state"] == 42
    assert meta["recipe"]["model"]["kind"] == model["kind"] and meta["recipe"]["calibrate"] == calibrate
    assert [s["kind"] for s in meta["recipe"]["steps"]] == ["clip_outliers", "standard_scaler"]
    assert meta["recipe"]["steps"][0]["params"] == {"lower_pct": 1.0, "upper_pct": 99.0}  # defaults are written out
    assert len(meta["cv_folds"]) == 4 and all(f["train_rows"] > 0 and f["test_rows"] > 0 for f in meta["cv_folds"])
    assert meta["holdout"] is not None and meta["holdout"]["rows"] > 0 and meta["holdout"]["accuracy"] > 0
    assert meta["trading"] is not None
    imp = meta["feature_importance"]
    assert imp and {i["feature"] for i in imp} <= set(FEATURES) and abs(sum(i["importance"] for i in imp) - 1) < 1e-6
    assert imp[0]["feature"] == "ret_1"  # the only informative feature
    assert 0 < result.metrics["accuracy"] <= 1


def test_trainer_without_importances_for_pca_and_with_selected_ones(tmp_path, dataset):
    pca = train(tmp_path, dataset, recipe(steps=[{"kind": "pca", "params": {"n_components": 0.99}}]))
    assert json.loads(pca.metadata_path.read_text())["feature_importance"] == []
    picked = train(tmp_path, dataset, recipe(steps=[{"kind": "select_k_best", "params": {"k": 2}}]))
    names = [i["feature"] for i in json.loads(picked.metadata_path.read_text())["feature_importance"]]
    assert len(names) == 2 and "ret_1" in names


def test_logistic_importances_use_standardised_coefficients_not_raw_scale():
    """A tiny-scale noise feature must not outrank a large-scale informative one just because of its unit."""
    rng = np.random.default_rng(0)
    n = 600
    ret = rng.normal(0, 1e-2, n)  # noise, ~1e-2 scale
    vol = rng.normal(1e6, 1e5, n)  # the only informative feature, ~1e6 scale
    x = np.column_stack([ret, vol])
    y = (vol > 1e6).astype(int)
    voting = {"kind": "voting", "voting": "soft", "members": [rf(20), {"algorithm": "logistic_regression", "params": {}}]}
    model = recipes.build(recipe(voting))
    model.fit(x, y)
    ranked = recipes.importances(model, ["ret", "vol"], x)
    assert [r["feature"] for r in ranked][0] == "vol" and ranked[0]["importance"] > 0.7
    # without the training matrix the logistic member (scale unknown) is left out; the forest still reports
    assert [r["feature"] for r in recipes.importances(model, ["ret", "vol"])][0] == "vol"
    logistic_only = recipes.build(recipe({"kind": "single", "algorithm": "logistic_regression", "params": {}}))
    logistic_only.fit(x, y)
    assert recipes.importances(logistic_only, ["ret", "vol"]) == []
    assert recipes.importances(logistic_only, ["ret", "vol"], x)[0]["feature"] == "vol"


def test_stacking_importances_map_through_each_members_selected_columns():
    x, y = frame(500, d=6, classes=2)
    steps = [{"kind": "select_k_best", "params": {"k": 2}}]
    model = recipes.build(recipe({"kind": "stacking", "blocks": 3, "members": [rf(15), rf(15)],
                                  "final": {"algorithm": "logistic_regression", "params": {}}}, steps=steps), embargo_rows=3)
    model.fit(x, y)
    names = [f"f{i}" for i in range(6)]
    ranked = recipes.importances(model, names, x)
    assert len(ranked) == 2 and ranked[0]["feature"] == "f0" and abs(sum(r["importance"] for r in ranked) - 1) < 1e-9


def test_trainer_embargo_covers_the_label_reach_of_interleaved_symbols(tmp_path, dataset):
    trainer = ModelTrainer()
    config = make_config(tmp_path, data_path=dataset, algorithm="recipe", hyperparams={}, recipe=recipe(STACKING, calibrate="sigmoid"))
    data = trainer._build_features(trainer._load_data(config), config)
    # two symbols interleaved by time step, label reaches 5 steps: the other symbol's row of the same
    # step plus 5 steps x 2 symbols after it
    assert trainer._embargo_rows(data) == 11
    solo = write_dataset(tmp_path / "solo.csv", symbols=("AAA",), kind="return_threshold")
    config1 = make_config(tmp_path, data_path=solo, algorithm="recipe", hyperparams={}, recipe=recipe(STACKING))
    assert trainer._embargo_rows(trainer._build_features(trainer._load_data(config1), config1)) == 5
    built = trainer._recipe_factory(trainer._prepare_recipe(config), {"random_state": 42}, 11)()
    assert built.embargo_rows == 11 and stacker_of(built.estimator).embargo_rows == 11


def test_trainer_checks_the_recipe_and_the_size(tmp_path, dataset):
    cfg = make_config(tmp_path, data_path=dataset, algorithm="recipe", hyperparams={})
    with pytest.raises(ValueError, match="needs a recipe"):
        ModelTrainer().train(cfg)
    cfg = make_config(tmp_path, data_path=dataset, algorithm="recipe", hyperparams={"n_estimators": 5}, recipe=recipe())
    with pytest.raises(ValueError, match="hyperparams must be empty"):
        ModelTrainer().train(cfg)
    cfg = make_config(tmp_path, data_path=dataset, algorithm="random_forest", recipe=recipe())
    with pytest.raises(ValueError, match="only be used with algorithm 'recipe'"):
        ModelTrainer().train(cfg)
    bad = recipe(steps=[{"kind": "select_k_best", "params": {"k": 0}}], model={"kind": "single", "algorithm": "random_forest", "params": {"bogus": 1}})
    with pytest.raises(ValueError, match=r"invalid recipe: steps\[0\]\.params\.k.*model\.params\.bogus"):
        ModelTrainer().train(make_config(tmp_path, data_path=dataset, algorithm="recipe", hyperparams={}, recipe=bad))

    # six huge forests together are refused although one of them alone would fit
    big = {"algorithm": "random_forest", "params": {"n_estimators": 5000}}
    one = recipes.prepare(recipe({"kind": "single", **big}))
    recipes.check_model_size(one, 2000, 3)
    many = recipe({"kind": "voting", "voting": "soft", "members": [big] * 6})
    larger = write_dataset(tmp_path / "larger.csv", steps=1500, kind="return_threshold")  # 3000 rows
    with pytest.raises(ValueError, match=r"totalling about .* GB \(limit 2 GB\)"):
        ModelTrainer().train(make_config(tmp_path, data_path=larger, algorithm="recipe", hyperparams={}, recipe=many))
    recipes.check_model_size(one, 3000, 3)  # the same forest alone is fine on that data
    assert not list((tmp_path / "models").glob("*.joblib"))


def test_trainer_main_accepts_a_recipe_payload(tmp_path, dataset):
    config = make_config(tmp_path, data_path=dataset, algorithm="recipe", hyperparams={}, recipe=recipe(), recipe_name="my_algo", n_splits=2)
    done = subprocess.run([sys.executable, "-m", "ml.trainer", "--config-json", json.dumps({**config.__dict__})],
                          cwd=APEX_PYTHON, capture_output=True, text=True)
    assert done.returncode == 0, done.stderr
    assert json.loads(done.stdout.strip().splitlines()[-1])["algorithm"] == "recipe"
    assert TrainingConfig(**{**config.__dict__}).recipe_name == "my_algo"


# ------------------------------------------------------------------
# Scoring through the app's entry point
# ------------------------------------------------------------------


def run_predict(*args: str) -> dict:
    out = subprocess.run([sys.executable, "-m", "ml.predict", *args], cwd=APEX_PYTHON, capture_output=True, text=True, check=True)
    return json.loads(out.stdout.strip().splitlines()[-1])


@pytest.mark.parametrize("model, calibrate", [
    ({"kind": "single", **rf()}, "none"),
    ({"kind": "voting", "voting": "soft", "members": [rf(), {"algorithm": "xgboost", "params": {"n_estimators": 8}}]}, "isotonic"),
    ({"kind": "voting", "voting": "hard", "members": [rf(), rf(8, 3)]}, "none"),
    (STACKING, "sigmoid"),
])
def test_predict_round_trip_in_a_fresh_interpreter(tmp_path, dataset, model, calibrate):
    result = train(tmp_path, dataset, recipe(model, steps=STEPS, calibrate=calibrate))
    rng = np.random.default_rng(0)
    rows = [[float(rng.normal(0, 0.02)), float(rng.uniform(20, 80)), float(abs(rng.normal(0.01, 0.005)))] for _ in range(20)]
    loaded = joblib.load(result.model_path)
    batch = run_predict("--model-path", str(result.model_path), "--features-matrix-json", json.dumps(rows))
    assert batch["signals"] == [int(p) for p in loaded.predict(np.array(rows))]
    if hasattr(loaded, "predict_proba"):
        expected = loaded.predict_proba(np.array(rows)).max(axis=1)
        assert np.allclose(batch["probabilities"], expected)
    else:
        assert batch["probabilities"] == [None] * 20
    for i in (0, 7, 19):  # single-row mode agrees with batch mode
        single = run_predict("--model-path", str(result.model_path), "--features-json", json.dumps(rows[i]))
        assert single["signal"] == batch["signals"][i]
        assert single["probability"] == pytest.approx(batch["probabilities"][i]) if batch["probabilities"][i] is not None else single["probability"] is None
    named = run_predict("--model-path", str(result.model_path), "--features-json", json.dumps(dict(zip(FEATURES, rows[3]))))
    assert named["signal"] == batch["signals"][3]


# ------------------------------------------------------------------
# Command line
# ------------------------------------------------------------------

PARAM_KEYS = {"key", "label", "type", "default", "help"}
PARAM_OPTIONAL = {"min", "max", "choices"}
CHECK_KEYS = {"ok", "errors", "warnings", "steps", "models", "est_model_mb"}


def cli(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, "-m", "ml.recipes", *args], cwd=APEX_PYTHON, capture_output=True, text=True)


def test_cli_blocks_matches_the_contract_shape():
    done = cli("blocks")
    assert done.returncode == 0, done.stderr
    out = json.loads(done.stdout)
    assert set(out) == {"steps", "members", "limits"}
    for step in out["steps"]:
        assert set(step) == {"kind", "label", "description", "group", "params"} and step["group"] in {"scale", "clean", "select", "reduce"}
        for p in step["params"]:
            assert PARAM_KEYS <= set(p) <= PARAM_KEYS | PARAM_OPTIONAL and p["type"] in {"int", "float", "bool", "choice", "text"}
    assert all(set(m) == {"id", "label", "description"} for m in out["members"])
    assert set(out["limits"]) == {"max_steps", "max_members", "min_members", "stacking_blocks"}
    assert out == json.loads(json.dumps(recipes.blocks()))


def test_cli_blocks_agree_with_the_ui_mock():
    mock = REPO / "apex-ui" / "src" / "lib" / "mockMl.ts"
    if not mock.is_file():
        pytest.skip("UI sources not present")
    text = mock.read_text()
    section = text[text.index("export const MOCK_BLOCKS"):text.index("/** Mock validation")]
    kinds = re.findall(r"step\('(\w+)'", section)
    assert kinds == [s["kind"] for s in recipes.blocks()["steps"]]
    keys = {k: re.findall(r"key: '(\w+)'", seg) for k, seg in zip(kinds, re.split(r"\n    step\(", "\n" + section.split("steps: [", 1)[1].split("  ],\n  members")[0])[1:])}
    for step in recipes.blocks()["steps"]:
        assert keys[step["kind"]] == [p["key"] for p in step["params"]], step["kind"]
    limits = re.search(r"limits: \{ max_steps: (\d+), max_members: (\d+), min_members: (\d+), stacking_blocks: \[(\d+), (\d+)\]", section)
    assert [int(v) for v in limits.groups()] == [8, 6, 2, 3, 8]


def test_cli_validate_prints_a_check_and_exits_zero_even_when_invalid():
    good = cli("validate", "--json", json.dumps(recipe()))
    assert good.returncode == 0 and set(json.loads(good.stdout)) == CHECK_KEYS and json.loads(good.stdout)["ok"] is True
    bad = cli("validate", "--json", json.dumps(recipe(steps=[{"kind": "pca", "params": {"n_components": 0}}])))
    out = json.loads(bad.stdout)
    assert bad.returncode == 0 and out["ok"] is False and set(out) == CHECK_KEYS
    assert out["errors"][0]["path"] == "steps[0].params.n_components" and set(out["errors"][0]) == {"path", "message"}
    junk = cli("validate", "--json", "{not json")
    out = json.loads(junk.stdout)
    assert junk.returncode == 0 and out["ok"] is False and set(out) == CHECK_KEYS and out["errors"][0]["path"] == ""
