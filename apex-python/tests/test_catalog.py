"""Algorithm catalog and hyperparameter validation."""

from __future__ import annotations

import inspect
import json
import subprocess
import sys
from pathlib import Path

import pytest

from ml import catalog
from ml.catalog import MAX_JOBS, algorithms, estimator_class, param_specs, supported_ids, validate_hyperparams
from ml.trainer import ModelTrainer
from tests.test_trainer import make_config

APEX_PYTHON = Path(__file__).resolve().parents[1]
BUILTIN = [a for a in supported_ids() if a != "custom"]
#: Parameters that must never be offered: objects, callables, or controlled by the application.
OBJECT_PARAMS = {"base_estimator", "estimator", "init", "callbacks", "n_jobs", "verbose", "warm_start"}


def _independent_param_names(algorithm: str) -> set[str]:
    """Constructor parameters read straight from the library, without using catalog.py."""
    cls = estimator_class(algorithm)
    if algorithm == "xgboost":
        return set(cls().get_params())
    params = inspect.signature(cls.__init__).parameters.values()
    return {p.name for p in params if p.name != "self" and p.kind in (p.POSITIONAL_OR_KEYWORD, p.KEYWORD_ONLY)}


def test_supported_ids_cover_the_requested_algorithms():
    ids = supported_ids()
    for expected in ("random_forest", "extra_trees", "gradient_boosting", "hist_gradient_boosting", "logistic_regression", "custom"):
        assert expected in ids
    try:
        import xgboost  # noqa: F401

        assert "xgboost" in ids
    except ImportError:
        assert "xgboost" not in ids


@pytest.mark.parametrize("algorithm", BUILTIN)
def test_every_constructor_parameter_is_listed_or_deliberately_hidden(algorithm):
    library = _independent_param_names(algorithm)
    listed = set(param_specs(algorithm))
    hidden = set(catalog.hidden_params(algorithm))
    assert listed | hidden == library, f"neither listed nor hidden: {library - listed - hidden}"
    assert not listed & hidden
    # Anything hidden is an object/callable/controlled parameter, a deprecated one, or a categorical option.
    allowed_hidden = set(catalog.HIDDEN) | {"criterion", "penalty", "interaction_constraints", "monotone_constraints"}
    assert hidden <= allowed_hidden, f"unexpected hidden parameters: {hidden - allowed_hidden}"
    # The headline tuning parameters are all there.
    for key in ("n_estimators", "max_depth", "learning_rate", "max_iter", "C", "min_samples_leaf", "class_weight", "random_state"):
        if key in library and key not in hidden:
            assert key in listed


@pytest.mark.parametrize("algorithm", BUILTIN)
def test_object_parameters_are_not_exposed(algorithm):
    keys = set(param_specs(algorithm))
    assert not keys & OBJECT_PARAMS, keys & OBJECT_PARAMS


@pytest.mark.parametrize("algorithm", [a for a in BUILTIN if a != "xgboost"])
def test_defaults_match_the_library(algorithm):
    defaults = estimator_class(algorithm)().get_params()
    for key, spec in param_specs(algorithm).items():
        if key == "random_state":
            assert spec["default"] == 42  # the trainer's fixed seed
        else:
            assert spec["default"] == defaults[key], key


def test_catalog_entries_are_well_formed_and_json_safe():
    data = algorithms()
    assert [a["id"] for a in data] == supported_ids()
    json.dumps(data, allow_nan=False)
    for algo in data:
        assert algo["label"] and algo["description"] and isinstance(algo["accepts_extra_params"], bool)
        for p in algo["params"]:
            assert p["type"] in {"int", "float", "bool", "choice", "text"}
            assert p["key"] and p["label"] and p["help"]
            if p["type"] == "choice":
                assert p["choices"] and all(isinstance(c, str) for c in p["choices"])
            if "min" in p and "max" in p:
                assert p["min"] <= p["max"]
    assert next(a for a in data if a["id"] == "custom")["params"] == []


def test_module_prints_the_catalog_as_json():
    out = subprocess.run([sys.executable, "-m", "ml.catalog"], cwd=APEX_PYTHON, capture_output=True, text=True, check=True)
    assert json.loads(out.stdout.strip().splitlines()[-1]) == json.loads(json.dumps(algorithms()))


# ------------------------------------------------------------------
# Validator
# ------------------------------------------------------------------


def test_parameters_the_old_allow_list_did_not_know_are_accepted():
    assert validate_hyperparams("random_forest", {"min_samples_split": 6, "bootstrap": False, "criterion": "entropy", "ccp_alpha": 0.001}) == {
        "min_samples_split": 6, "bootstrap": False, "criterion": "entropy", "ccp_alpha": 0.001,
    }
    assert validate_hyperparams("extra_trees", {"max_features": 0.5, "n_estimators": 50})["max_features"] == 0.5
    assert validate_hyperparams("random_forest", {"max_features": 3})["max_features"] == 3
    assert validate_hyperparams("hist_gradient_boosting", {"max_iter": 40, "l2_regularization": 1, "early_stopping": "false"}) == {
        "max_iter": 40, "l2_regularization": 1.0, "early_stopping": False,
    }
    assert validate_hyperparams("logistic_regression", {"solver": "liblinear", "fit_intercept": False})
    assert validate_hyperparams("gradient_boosting", {"learning_rate": 2, "n_iter_no_change": 5, "loss": "exponential"})["learning_rate"] == 2.0
    if "xgboost" in supported_ids():
        assert validate_hyperparams("xgboost", {"reg_lambda": 2, "min_child_weight": 3, "tree_method": "hist", "gamma": 0.5})


def test_values_are_coerced_to_the_declared_type():
    out = validate_hyperparams("random_forest", {"n_estimators": 50.0, "min_samples_leaf": 3.0, "max_depth": None})
    assert out == {"n_estimators": 50, "min_samples_leaf": 3, "max_depth": None}
    assert type(out["n_estimators"]) is int
    assert type(validate_hyperparams("gradient_boosting", {"learning_rate": 1})["learning_rate"]) is float


@pytest.mark.parametrize("algorithm", [a for a in BUILTIN])
def test_unknown_keys_are_rejected_and_the_message_lists_the_valid_ones(algorithm):
    with pytest.raises(ValueError) as err:
        validate_hyperparams(algorithm, {"definitely_not_a_parameter": 1})
    message = str(err.value)
    assert "unknown hyperparameter 'definitely_not_a_parameter'" in message
    assert "valid: " in message
    for key in list(param_specs(algorithm))[:5]:
        assert key in message


@pytest.mark.parametrize("key", ["init", "base_estimator", "callbacks", "estimator", "monotonic_cst"])
def test_object_parameters_are_rejected_as_unknown(key):
    for algorithm in BUILTIN:
        if key in param_specs(algorithm):
            continue
        with pytest.raises(ValueError, match=f"unknown hyperparameter '{key}'"):
            validate_hyperparams(algorithm, {key: "x"})


@pytest.mark.parametrize(
    "algorithm, params, fragment",
    [
        ("random_forest", {"bootstrap": "yes"}, "bootstrap"),
        ("random_forest", {"criterion": "mse"}, "criterion"),
        ("random_forest", {"max_depth": 0}, "max_depth"),
        ("random_forest", {"min_samples_leaf": 1.5}, "min_samples_leaf"),
        ("random_forest", {"min_weight_fraction_leaf": 0.9}, "min_weight_fraction_leaf"),
        ("random_forest", {"max_features": 0}, "max_features"),
        ("random_forest", {"n_estimators": float("inf")}, "n_estimators"),
        ("random_forest", {"n_estimators": None}, "n_estimators"),
        ("random_forest", {"n_estimators": [1]}, "n_estimators"),
        ("hist_gradient_boosting", {"max_bins": 300}, "max_bins"),
        ("hist_gradient_boosting", {"early_stopping": "maybe"}, "early_stopping"),
        ("logistic_regression", {"solver": "magic"}, "solver"),
        ("logistic_regression", {"C": 0}, "'C'"),
        ("gradient_boosting", {"subsample": 1.5}, "subsample"),
    ],
)
def test_invalid_values_are_rejected_with_the_parameter_named(algorithm, params, fragment):
    with pytest.raises(ValueError) as err:
        validate_hyperparams(algorithm, params)
    assert fragment in str(err.value)
    assert "expected" in str(err.value)


@pytest.mark.parametrize(
    "algorithm, key, cap",
    [
        ("random_forest", "n_estimators", 5000),
        ("extra_trees", "n_estimators", 5000),
        ("gradient_boosting", "n_estimators", 5000),
        ("random_forest", "max_depth", 200),
        ("gradient_boosting", "max_depth", 200),
        ("hist_gradient_boosting", "max_iter", 100_000),
        ("hist_gradient_boosting", "max_depth", 200),
        ("logistic_regression", "max_iter", 100_000),
        ("random_forest", "max_leaf_nodes", 100_000),
    ],
)
def test_cost_parameters_are_capped(algorithm, key, cap):
    assert validate_hyperparams(algorithm, {key: cap})[key] == cap
    with pytest.raises(ValueError, match=key):
        validate_hyperparams(algorithm, {key: cap + 1})


@pytest.mark.skipif("xgboost" not in supported_ids(), reason="xgboost not installed")
def test_xgboost_caps():
    assert validate_hyperparams("xgboost", {"n_estimators": 5000, "max_depth": 200})
    for params in ({"n_estimators": 5001}, {"max_depth": 201}, {"num_parallel_tree": 101}):
        with pytest.raises(ValueError):
            validate_hyperparams("xgboost", params)


def test_n_jobs_is_clamped_never_passed_through():
    for given, expected in ((1, 1), (2, 2), (4, 4), (16, MAX_JOBS), (1000, MAX_JOBS), (-1, MAX_JOBS)):
        assert validate_hyperparams("random_forest", {"n_jobs": given}) == {"n_jobs": expected}
    for bad in (0, -2, 1.5, "all", True, None):
        with pytest.raises(ValueError, match="n_jobs"):
            validate_hyperparams("random_forest", {"n_jobs": bad})


def test_too_many_keys_and_bad_containers_are_rejected():
    with pytest.raises(ValueError, match="too many"):
        validate_hyperparams("custom", {f"k{i}": 1 for i in range(catalog.MAX_PARAM_KEYS + 1)})
    with pytest.raises(ValueError, match="object"):
        validate_hyperparams("random_forest", [("n_estimators", 5)])  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="Unknown algorithm"):
        validate_hyperparams("svm", {})


def test_custom_parameters_are_free_form_but_json_scalars_only():
    ok = {"k": 3, "metric": "euclid", "weights": [1, 2, 3], "flag": True, "none": None, "x": 0.5}
    assert validate_hyperparams("custom", ok) == ok
    for bad in ({"k": {"nested": 1}}, {"k": [[1]]}, {"1bad": 1}, {"k": "x" * 201}, {"k": float("nan")}, {"a b": 1}):
        with pytest.raises(ValueError):
            validate_hyperparams("custom", bad)


def test_the_librarys_own_parameter_check_is_a_second_line_of_defence():
    # Bypass the range check: scikit-learn itself must still refuse, as a ValueError naming the algorithm.
    with pytest.raises(ValueError, match="random_forest"):
        catalog._check_with_library("random_forest", {"n_estimators": 0})
    with pytest.raises(ValueError, match="logistic_regression"):
        catalog._check_with_library("logistic_regression", {"C": -3.0})
    catalog._check_with_library("random_forest", {"n_estimators": 3})


@pytest.mark.parametrize(
    "algorithm, params",
    [
        ("extra_trees", {"n_estimators": 8, "max_depth": 3, "criterion": "entropy"}),
        ("hist_gradient_boosting", {"max_iter": 10, "max_depth": 3}),
        ("random_forest", {"n_estimators": 8, "bootstrap": False, "min_samples_split": 10}),
    ],
)
def test_new_algorithms_and_parameters_really_train(tmp_path, algorithm, params):
    config = make_config(tmp_path, algorithm=algorithm, hyperparams=params, n_splits=2)
    result = ModelTrainer().train(config)
    meta = json.loads(result.metadata_path.read_text())
    assert meta["algorithm"] == algorithm
    for key, value in params.items():
        assert meta["hyperparams"][key] == value
    assert meta["hyperparams"]["random_state"] == 42


# ------------------------------------------------------------------
# Model size guard
# ------------------------------------------------------------------


def test_huge_forests_are_refused_before_fitting():
    from ml.catalog import check_model_size

    # The verifier's examples: valid parameters that would need many gigabytes.
    with pytest.raises(ValueError, match="GB"):
        check_model_size("random_forest", {"n_estimators": 5000}, rows=100_000, classes=2)
    with pytest.raises(ValueError, match="GB"):
        check_model_size("hist_gradient_boosting", {"max_iter": 100000, "max_leaf_nodes": 100000}, rows=100_000, classes=3)
    with pytest.raises(ValueError, match="GB"):
        check_model_size("xgboost", {"n_estimators": 5000, "num_parallel_tree": 100}, rows=50_000, classes=2)


def test_ordinary_configurations_and_bounded_trees_are_allowed():
    from ml.catalog import check_model_size

    check_model_size("random_forest", {}, rows=100_000, classes=2)
    check_model_size("random_forest", {"n_estimators": 500}, rows=5_000, classes=3)
    # Big forests are fine when each tree is bounded.
    check_model_size("random_forest", {"n_estimators": 2000, "max_depth": 8}, rows=500_000, classes=2)
    check_model_size("gradient_boosting", {"n_estimators": 300}, rows=20_000, classes=3)
    check_model_size("logistic_regression", {"max_iter": 100000}, rows=1_000_000, classes=3)
    # Custom code cannot be bounded here; it has the time limit instead.
    check_model_size("custom", {}, rows=10**7, classes=2)


def test_the_estimate_tracks_a_real_forest():
    import io

    import joblib
    import numpy as np
    from sklearn.ensemble import RandomForestClassifier

    from ml.catalog import estimate_model_bytes

    rng = np.random.default_rng(0)
    x = rng.normal(size=(8000, 10))
    y = rng.integers(0, 2, 8000)
    model = RandomForestClassifier(n_estimators=6, random_state=0, n_jobs=1).fit(x, y)
    buf = io.BytesIO()
    joblib.dump(model, buf)
    est = estimate_model_bytes("random_forest", {"n_estimators": 6}, rows=8000, classes=2)
    # Deliberately pessimistic, but within a small factor of reality.
    assert buf.tell() <= est <= buf.tell() * 4, (buf.tell(), est)


def test_the_trainer_applies_the_guard_before_fitting(tmp_path):
    from ml.trainer import ModelTrainer

    from tests.test_trainer import make_config

    config = make_config(tmp_path, hyperparams={"n_estimators": 5000}, n_splits=2)
    # A tiny dataset is fine, so shrink the ceiling to prove the guard is in the path.
    import ml.catalog as catalog

    old = catalog.MAX_MODEL_BYTES
    catalog.MAX_MODEL_BYTES = 1000
    try:
        with pytest.raises(ValueError, match="GB"):
            ModelTrainer().train(config)
    finally:
        catalog.MAX_MODEL_BYTES = old
    assert not list(tmp_path.glob("models/*.joblib")), "nothing was fitted or saved"
