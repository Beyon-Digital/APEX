"""Algorithm catalog and hyperparameter validation.

The catalog is built by introspecting the estimators installed in this environment, so it always
matches the library version that will actually train the model:

* every constructor parameter of a supported estimator is listed (``inspect.signature``), with
  its default (``get_params``) and, where scikit-learn publishes them, its allowed range or
  choices (``_parameter_constraints``);
* parameters that take objects or callables (``init``, ``base_estimator``, ``callbacks`` ...) and
  parameters the application controls itself (``n_jobs``, ``verbose`` ...) are not exposed.

``python -m ml.catalog`` prints the catalog as a JSON array of ``AlgorithmDto``.

:func:`validate_hyperparams` is the single authority on what a training request may pass to an
estimator: any exposed constructor parameter is accepted, values are coerced from JSON to the
declared type, unknown keys are rejected with the list of valid ones, and cost parameters have
hard caps so a request cannot ask for an unbounded amount of work.
"""

from __future__ import annotations

import importlib
import inspect
import json
import math
import re
from functools import lru_cache
from typing import Any

MAX_SEED = 2**31 - 1
DEFAULT_SEED = 42
MAX_PARAM_KEYS = 60
MAX_TEXT = 200
MAX_LIST = 100
#: ``n_jobs`` is never exposed; a request that sets it is clamped to this many workers.
MAX_JOBS = 4
#: Integers with no tighter bound must still be sane.
GENERIC_INT_LIMIT = 10**9

#: Upper bounds on the parameters that decide how much work a fit does.
CAPS: dict[str, int] = {
    "n_estimators": 5000,
    "max_iter": 100_000,
    "max_depth": 200,
    "max_leaf_nodes": 100_000,
    "max_leaves": 100_000,
    "max_bins": 255,
    "max_bin": 4096,
    "n_iter_no_change": 1000,
    "num_parallel_tree": 100,
    "max_features": 100_000,
    "max_samples": 1_000_000_000,
}

#: Parameters that take objects/callables, are meaningless for a one-shot fit, or that the
#: application sets itself. They are not part of the catalog and are rejected if requested
#: (``n_jobs`` is special: accepted, but clamped).
HIDDEN: frozenset[str] = frozenset(
    {
        "n_jobs", "nthread", "verbose", "verbosity", "warm_start",
        "base_estimator", "estimator", "init", "callbacks", "scoring",
        "monotonic_cst", "interaction_cst", "categorical_features", "feature_types", "feature_weights",
        "eval_metric", "objective", "early_stopping_rounds", "enable_categorical", "device",
        "validate_parameters", "importance_type", "missing", "kwargs", "args",
        "max_cat_to_onehot", "max_cat_threshold",
    }
)

_OBJECT_ANNOTATION = re.compile(r"Callable|Sequence|List|Dict|Mapping|Booster|Iterable|Tuple|ndarray|Proto")

_NAME_OK = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,60}$")

_ALGORITHMS: dict[str, dict[str, str]] = {
    "random_forest": {
        "label": "Random forest",
        "description": "Many decision trees voting. Robust and hard to overfit badly.",
        "module": "sklearn.ensemble",
        "cls": "RandomForestClassifier",
    },
    "extra_trees": {
        "label": "Extra trees",
        "description": "Like a random forest with random split points. Faster, often smoother.",
        "module": "sklearn.ensemble",
        "cls": "ExtraTreesClassifier",
    },
    "gradient_boosting": {
        "label": "Gradient boosting",
        "description": "Trees added one at a time to fix earlier errors.",
        "module": "sklearn.ensemble",
        "cls": "GradientBoostingClassifier",
    },
    "hist_gradient_boosting": {
        "label": "Histogram gradient boosting",
        "description": "Fast gradient boosting for larger datasets.",
        "module": "sklearn.ensemble",
        "cls": "HistGradientBoostingClassifier",
    },
    "logistic_regression": {
        "label": "Logistic regression",
        "description": "A linear model on standardised features. Simple, fast, a good baseline.",
        "module": "sklearn.linear_model",
        "cls": "LogisticRegression",
    },
    "xgboost": {
        "label": "XGBoost",
        "description": "Boosted trees with strong defaults.",
        "module": "xgboost",
        "cls": "XGBClassifier",
    },
}

CUSTOM = "custom"
_CUSTOM_INFO = {
    "label": "Custom estimator (Python file)",
    "description": "Your own scikit-learn style classifier from estimators/<name>.py. "
    "The file defines make_estimator(params) and receives the parameters you enter here.",
}

#: Estimators with no keyword defaults of their own: the documented library defaults.
_XGB_DEFAULTS: dict[str, Any] = {
    "n_estimators": 100, "max_depth": 6, "max_leaves": 0, "max_bin": 256, "grow_policy": "depthwise",
    "learning_rate": 0.3, "booster": "gbtree", "tree_method": "auto", "gamma": 0.0, "min_child_weight": 1.0,
    "max_delta_step": 0.0, "subsample": 1.0, "sampling_method": "uniform", "colsample_bytree": 1.0,
    "colsample_bylevel": 1.0, "colsample_bynode": 1.0, "reg_alpha": 0.0, "reg_lambda": 1.0,
    "scale_pos_weight": 1.0, "num_parallel_tree": 1,
}

#: Ranges for parameters whose library publishes none.
_XGB_RANGES: dict[str, tuple[float | None, float | None, bool, bool]] = {
    # (min, max, min_exclusive, max_exclusive)
    "n_estimators": (1, 5000, False, False),
    "max_depth": (0, 200, False, False),
    "learning_rate": (0.0, 1.0, False, False),
    "gamma": (0.0, None, False, False),
    "min_child_weight": (0.0, None, False, False),
    "max_delta_step": (0.0, None, False, False),
    "subsample": (0.0, 1.0, True, False),
    "colsample_bytree": (0.0, 1.0, True, False),
    "colsample_bylevel": (0.0, 1.0, True, False),
    "colsample_bynode": (0.0, 1.0, True, False),
    "reg_alpha": (0.0, None, False, False),
    "reg_lambda": (0.0, None, False, False),
    "scale_pos_weight": (0.0, None, True, False),
    "max_leaves": (0, 100_000, False, False),
    "max_bin": (2, 4096, False, False),
    "num_parallel_tree": (1, 100, False, False),
}

#: Parameters whose accepted values cannot be read from the library.
_CHOICES: dict[str, list[str]] = {
    "booster": ["gbtree", "dart"],
    "tree_method": ["auto", "exact", "approx", "hist"],
    "grow_policy": ["depthwise", "lossguide"],
    "sampling_method": ["uniform", "gradient_based"],
    "multi_strategy": ["one_output_per_tree", "multi_output_tree"],
}

_HELP: dict[str, str] = {
    "n_estimators": "Number of trees (or boosting rounds). More is slower and rarely helps past a few hundred.",
    "max_iter": "Maximum number of iterations. Raise it if training warns that it did not converge.",
    "max_depth": "Deepest a tree may grow. Leave empty for no limit. Smaller values generalise better.",
    "max_leaf_nodes": "Cap on the leaves of each tree. Leave empty for no limit.",
    "max_leaves": "Cap on the leaves of each tree (0 = no limit).",
    "min_samples_leaf": "Smallest number of rows a leaf may hold. Larger values smooth the model.",
    "min_samples_split": "Smallest number of rows a node needs before it may be split.",
    "min_weight_fraction_leaf": "Smallest share of the total sample weight a leaf must hold.",
    "min_impurity_decrease": "A split must reduce impurity by at least this much.",
    "min_child_weight": "Minimum total weight needed in a child node (higher = more cautious).",
    "max_features": "Features considered at each split: 'sqrt', 'log2', a count (1 or more) or a share in (0, 1].",
    "max_samples": "Rows drawn for each tree when bootstrapping: a share in (0, 1] or a count.",
    "class_weight": "'balanced' weighs classes inversely to their frequency, which helps with rare classes.",
    "criterion": "How split quality is measured.",
    "bootstrap": "Draw each tree's rows with replacement.",
    "oob_score": "Estimate accuracy on the rows left out of each bootstrap sample.",
    "ccp_alpha": "Pruning strength. Larger values prune more.",
    "learning_rate": "Step size of each boosting round. Smaller needs more rounds but is steadier.",
    "subsample": "Share of rows used by each round, in (0, 1].",
    "colsample_bytree": "Share of features used by each tree, in (0, 1].",
    "colsample_bylevel": "Share of features used at each tree level, in (0, 1].",
    "colsample_bynode": "Share of features used at each split, in (0, 1].",
    "loss": "Loss function that boosting minimises.",
    "validation_fraction": "Share of training rows held back to decide when to stop early.",
    "n_iter_no_change": "Stop when the validation score has not improved for this many rounds. Leave empty to disable.",
    "early_stopping": "Stop adding rounds when a validation score stops improving: 'auto', 'true' or 'false'.",
    "tol": "Stop when improvement falls below this value.",
    "l2_regularization": "L2 penalty on leaf values.",
    "max_bins": "Number of bins used to bucket each feature (2 to 255).",
    "C": "Inverse regularisation strength. Smaller means stronger regularisation.",
    "l1_ratio": "Mix of L1 and L2 regularisation: 0 is pure L2, 1 is pure L1.",
    "solver": "Optimisation algorithm. Some solvers do not support every regularisation.",
    "fit_intercept": "Add a constant term to the model.",
    "dual": "Use the dual formulation (only for some solvers).",
    "intercept_scaling": "Scale of the synthetic intercept feature (liblinear only).",
    "random_state": "Seed for the random choices. Defaults to 42 so a run can be repeated exactly.",
    "gamma": "Minimum loss reduction needed to split a node.",
    "reg_alpha": "L1 penalty on leaf weights.",
    "reg_lambda": "L2 penalty on leaf weights.",
    "scale_pos_weight": "Weight of the positive class against the negative class.",
    "max_delta_step": "Largest step allowed for each tree's output (0 = no limit).",
    "max_bin": "Number of bins used by the histogram tree method.",
    "tree_method": "Tree construction algorithm.",
    "booster": "Boosting algorithm.",
    "grow_policy": "How new nodes are added to a tree.",
    "sampling_method": "How rows are sampled in each round.",
    "num_parallel_tree": "Trees built in each round (more than 1 makes a boosted random forest).",
}


# ------------------------------------------------------------------
# Introspection
# ------------------------------------------------------------------


def supported_ids() -> list[str]:
    """Algorithm ids usable in this environment (xgboost only when it can be imported)."""
    ids = []
    for algorithm in _ALGORITHMS:
        if algorithm == "xgboost" and not _xgboost_importable():
            continue
        ids.append(algorithm)
    return [*ids, CUSTOM]


@lru_cache(maxsize=1)
def _xgboost_importable() -> bool:
    try:
        importlib.import_module("xgboost")
        return True
    except Exception:  # noqa: BLE001 - optional dependency, any import failure means unavailable
        return False


def estimator_class(algorithm: str) -> type:
    info = _ALGORITHMS.get(algorithm)
    if info is None:
        raise ValueError(f"Unknown algorithm '{algorithm}'. Supported: {supported_ids()}")
    return getattr(importlib.import_module(info["module"]), info["cls"])


def constructor_params(cls: type) -> dict[str, inspect.Parameter]:
    """Every named constructor parameter of ``cls``.

    A class whose ``__init__`` forwards ``**kwargs`` (XGBoost) takes its real parameters from the
    base class, so those are followed along the MRO.
    """
    found: dict[str, inspect.Parameter] = {}
    for klass in cls.__mro__:
        init = klass.__dict__.get("__init__")
        if init is None:
            continue
        has_var_keyword = False
        for name, param in inspect.signature(init).parameters.items():
            if name == "self":
                continue
            if param.kind is inspect.Parameter.VAR_KEYWORD:
                has_var_keyword = True
            elif param.kind is not inspect.Parameter.VAR_POSITIONAL:
                found.setdefault(name, param)
        if not has_var_keyword:
            break
    return found


def _default_of(param: inspect.Parameter, cls: type, name: str, algorithm: str) -> Any:
    if param.default is not inspect.Parameter.empty:
        value = param.default
        if value is None and algorithm == "xgboost" and name in _XGB_DEFAULTS:
            return _XGB_DEFAULTS[name]
        return value
    return _XGB_DEFAULTS.get(name) if algorithm == "xgboost" else None


def _constraints(cls: type, name: str) -> list[Any]:
    cons = getattr(cls, "_parameter_constraints", {}).get(name)
    if cons is None:
        return []
    out = []
    for c in cons:
        # Hidden(...) wraps values that are accepted but deprecated.
        out.append(getattr(c, "constraint", c) if type(c).__name__ == "Hidden" else c)
    return out


def _is_hidden(name: str, default: Any, annotation: Any) -> bool:
    if name in HIDDEN:
        return True
    if isinstance(default, str) and default == "deprecated":
        return True
    if annotation is not inspect.Parameter.empty and _OBJECT_ANNOTATION.search(str(annotation)):
        return True
    return False


def hidden_params(algorithm: str) -> list[str]:
    """Constructor parameters of ``algorithm`` that the catalog does not expose."""
    if algorithm == CUSTOM:
        return []
    cls = estimator_class(algorithm)
    specs = param_specs(algorithm)
    return sorted(n for n in constructor_params(cls) if n not in specs)


def _label(key: str) -> str:
    return key.replace("_", " ").strip().capitalize() if key != "C" else "C"


def _bound(value: Any) -> float | int | None:
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, float) and not math.isfinite(value):
        return None
    return value


def _spec(algorithm: str, cls: type, name: str, param: inspect.Parameter) -> dict[str, Any] | None:
    default = _default_of(param, cls, name, algorithm)
    if _is_hidden(name, default, param.annotation):
        return None
    cons = _constraints(cls, name)
    str_options = None
    for c in cons:
        if type(c).__name__ == "StrOptions":
            str_options = sorted(c.options)
    intervals = [c for c in cons if type(c).__name__ == "Interval"]
    nullable = any(c is None for c in cons) or default is None
    is_bool_const = "boolean" in cons

    spec: dict[str, Any] = {
        "key": name, "label": _label(name), "type": "text", "default": None,
        "min": None, "max": None, "choices": None, "help": "",
        "nullable": nullable, "numeric_alt": False, "bool_alt": False,
        "min_open": False, "max_open": False,
    }

    def apply_interval(iv: Any) -> None:
        spec["min"] = _bound(iv.left)
        spec["max"] = _bound(iv.right)
        spec["min_open"] = iv.closed in ("right", "neither")
        spec["max_open"] = iv.closed in ("left", "neither")

    def integral(iv: Any) -> bool:
        return getattr(iv.type, "__name__", "") == "Integral"

    int_ivs = [iv for iv in intervals if integral(iv)]
    real_ivs = [iv for iv in intervals if not integral(iv)]

    if name == "random_state":
        spec.update(type="int", min=0, max=MAX_SEED, nullable=True, default=DEFAULT_SEED)
    elif isinstance(default, bool) or (is_bool_const and not str_options):
        spec.update(type="bool", default=bool(default) if default is not None else False, nullable=False)
    elif str_options and (is_bool_const):
        spec.update(type="choice", choices=[*str_options, "true", "false"], default=default, bool_alt=True)
    elif isinstance(default, str) or (default is None and str_options):
        choices = _CHOICES.get(name) or str_options
        if choices:
            spec.update(type="choice", choices=list(choices), default=default)
            if intervals:  # e.g. max_features: 'sqrt', 'log2' or a number
                spec["numeric_alt"] = True
        else:
            spec.update(type="text", default=default)
    elif isinstance(default, int) and not isinstance(default, bool):
        if int_ivs or not real_ivs:
            spec["type"] = "int"
            if int_ivs:
                apply_interval(int_ivs[0])
        else:
            spec["type"] = "float"
            apply_interval(real_ivs[0])
        spec["default"] = default
    elif isinstance(default, float):
        spec["type"] = "float"
        spec["default"] = default
        if real_ivs:
            apply_interval(real_ivs[0])
        elif int_ivs:
            apply_interval(int_ivs[0])
    elif default is None:
        ann = str(param.annotation) if param.annotation is not inspect.Parameter.empty else ""
        if intervals:
            first = intervals[0]
            spec["type"] = "int" if integral(first) else "float"
            apply_interval(first)
        elif "int" in ann and "float" not in ann:
            spec["type"] = "int"
        elif "float" in ann:
            spec["type"] = "float"
        elif "bool" in ann:
            spec["type"] = "bool"
        elif name in _CHOICES:
            spec.update(type="choice", choices=list(_CHOICES[name]))
        elif "str" in ann or not ann:
            spec["type"] = "text"
        else:
            return None
    else:
        return None

    if algorithm == "xgboost" and name in _XGB_RANGES:
        lo, hi, lo_open, hi_open = _XGB_RANGES[name]
        spec.update(min=lo, max=hi, min_open=lo_open, max_open=hi_open)
    if spec["type"] == "int" and name in CAPS and (spec["max"] is None or spec["max"] > CAPS[name]):
        spec["max"], spec["max_open"] = CAPS[name], False
    if spec["type"] == "int" and spec["max"] is None:
        spec["max"], spec["max_open"] = GENERIC_INT_LIMIT, False
    if spec["type"] == "float" and name == "max_samples":
        spec["numeric_alt"] = True
    if name == "max_features" and spec["type"] == "choice":
        spec["numeric_alt"] = True
    spec["help"] = _HELP.get(name, f"Passes `{name}` to {cls.__name__}. See the scikit-learn documentation.")
    if algorithm == "xgboost" and name not in _HELP:
        spec["help"] = f"Passes `{name}` to XGBClassifier. See the XGBoost documentation."
    return spec


@lru_cache(maxsize=None)
def _specs_cached(algorithm: str) -> dict[str, dict[str, Any]]:
    cls = estimator_class(algorithm)
    out: dict[str, dict[str, Any]] = {}
    for name, param in constructor_params(cls).items():
        spec = _spec(algorithm, cls, name, param)
        if spec is not None:
            out[name] = spec
    return out


def param_specs(algorithm: str) -> dict[str, dict[str, Any]]:
    """Internal specs (with nullability/open-bound flags) for every exposed parameter."""
    if algorithm == CUSTOM:
        return {}
    return _specs_cached(algorithm)


def _public(spec: dict[str, Any]) -> dict[str, Any]:
    out = {k: spec[k] for k in ("key", "label", "type", "default", "help")}
    if spec["min"] is not None:
        out["min"] = spec["min"]
    if spec["max"] is not None:
        out["max"] = spec["max"]
    if spec["choices"] is not None:
        out["choices"] = spec["choices"]
    return out


def algorithms() -> list[dict[str, Any]]:
    """The catalog as plain data (``AlgorithmDto[]``)."""
    out = []
    for algorithm in supported_ids():
        if algorithm == CUSTOM:
            out.append({"id": CUSTOM, **_CUSTOM_INFO, "params": [], "accepts_extra_params": True})
            continue
        info = _ALGORITHMS[algorithm]
        out.append(
            {
                "id": algorithm,
                "label": info["label"],
                "description": info["description"],
                "params": [_public(s) for s in param_specs(algorithm).values()],
                "accepts_extra_params": True,
            }
        )
    return out


# ------------------------------------------------------------------
# Validation
# ------------------------------------------------------------------


def _is_number(v: Any) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def _fmt(x: Any) -> str:
    return f"{x:g}" if isinstance(x, float) else str(x)


def _expected(spec: dict[str, Any]) -> str:
    t = spec["type"]
    if t == "bool":
        return "true or false"
    if t == "choice":
        opts = ", ".join(repr(c) for c in spec["choices"])
        text = f"one of {opts}"
        if spec["numeric_alt"]:
            text += ", or a number (a count of 1 or more, or a share in (0, 1])"
        if spec["nullable"]:
            text += ", or null"
        return text
    if t == "text":
        return f"text of at most {MAX_TEXT} characters" + (" or null" if spec["nullable"] else "")
    lo, hi = spec["min"], spec["max"]
    kind = "an integer" if t == "int" else "a number"
    if lo is not None and hi is not None:
        text = f"{kind} {'above' if spec['min_open'] else 'from'} {_fmt(lo)} {'and below' if spec['max_open'] else 'to'} {_fmt(hi)}"
    elif lo is not None:
        text = f"{kind} {'greater than' if spec['min_open'] else 'of at least'} {_fmt(lo)}"
    elif hi is not None:
        text = f"{kind} {'below' if spec['max_open'] else 'of at most'} {_fmt(hi)}"
    else:
        text = kind
    return text + (" or null" if spec["nullable"] else "")


def _in_range(v: float, spec: dict[str, Any]) -> bool:
    lo, hi = spec["min"], spec["max"]
    if lo is not None and (v <= lo if spec["min_open"] else v < lo):
        return False
    if hi is not None and (v >= hi if spec["max_open"] else v > hi):
        return False
    return True


def _coerce(spec: dict[str, Any], value: Any) -> Any:
    """Convert a JSON value to the spec's type, or raise ``ValueError``."""
    t = spec["type"]
    if value is None:
        if spec["nullable"]:
            return None
        raise ValueError
    if t == "bool":
        if isinstance(value, bool):
            return value
        raise ValueError
    if t == "int":
        if _is_number(value) and float(value).is_integer() and _in_range(value, spec):
            return int(value)
        raise ValueError
    if t == "float":
        if _is_number(value) and _in_range(value, spec):
            return float(value)
        if spec["numeric_alt"] and _is_number(value) and float(value).is_integer() and 1 <= value <= CAPS.get(spec["key"], GENERIC_INT_LIMIT):
            return int(value)
        raise ValueError
    if t == "choice":
        if isinstance(value, str) and value in spec["choices"]:
            if spec["bool_alt"] and value in ("true", "false"):
                return value == "true"
            return value
        if spec["numeric_alt"] and _is_number(value):
            if isinstance(value, int) or float(value).is_integer() and value >= 1:
                if 1 <= value <= CAPS.get(spec["key"], GENERIC_INT_LIMIT):
                    return int(value)
            elif 0 < value <= 1:
                return float(value)
        raise ValueError
    if t == "text":
        if isinstance(value, str) and len(value) <= MAX_TEXT:
            return value
        raise ValueError
    raise ValueError


def _clamp_jobs(value: Any) -> int:
    if not (_is_number(value) and float(value).is_integer()) or value == 0 or value < -1:
        raise ValueError
    return MAX_JOBS if value == -1 else min(int(value), MAX_JOBS)


def _validate_custom(hp: dict[str, Any]) -> dict[str, Any]:
    """Free-form parameters for a custom estimator: JSON scalars and short lists of them."""

    def scalar_ok(v: Any) -> bool:
        return v is None or isinstance(v, bool) or _is_number(v) or (isinstance(v, str) and len(v) <= MAX_TEXT)

    out: dict[str, Any] = {}
    for key, value in hp.items():
        if not isinstance(key, str) or not _NAME_OK.match(key):
            raise ValueError(f"invalid parameter name {key!r} for custom: use letters, digits and '_' (max 61 characters)")
        ok = scalar_ok(value) or (isinstance(value, list) and len(value) <= MAX_LIST and all(scalar_ok(v) for v in value))
        if not ok:
            raise ValueError(
                f"invalid value {value!r} for hyperparameter '{key}' of custom: "
                "expected a number, text, true/false, null or a list of those"
            )
        out[key] = value
    return out


def validate_hyperparams(algorithm: str, hyperparams: dict[str, Any] | None) -> dict[str, Any]:
    """Return the checked (and normalised) hyperparameters, or raise ``ValueError``.

    Any exposed constructor parameter of the estimator is accepted. Values are coerced to the
    declared type and range, unknown keys are rejected with the list of valid ones, and
    ``n_jobs`` is clamped (it is never passed through unbounded).
    """
    ids = supported_ids()
    if algorithm not in ids:
        raise ValueError(f"Unknown algorithm '{algorithm}'. Supported: {sorted(ids)}")
    hp = hyperparams or {}
    if not isinstance(hp, dict):
        raise ValueError("hyperparams must be an object")
    if len(hp) > MAX_PARAM_KEYS:
        raise ValueError(f"too many hyperparameters ({len(hp)}; max {MAX_PARAM_KEYS})")
    if algorithm == CUSTOM:
        return _validate_custom(hp)

    specs = param_specs(algorithm)
    out: dict[str, Any] = {}
    for key, value in hp.items():
        if key == "n_jobs":
            try:
                out[key] = _clamp_jobs(value)
            except ValueError:
                raise ValueError(
                    f"invalid value {value!r} for hyperparameter 'n_jobs' of {algorithm}: expected an integer from 1 to {MAX_JOBS} (or -1)"
                ) from None
            continue
        if key not in specs:
            raise ValueError(f"unknown hyperparameter '{key}' for {algorithm}; valid: {', '.join(specs)}")
        try:
            out[key] = _coerce(specs[key], value)
        except ValueError:
            raise ValueError(
                f"invalid value {value!r} for hyperparameter '{key}' of {algorithm}: expected {_expected(specs[key])}"
            ) from None
    _check_with_library(algorithm, out)
    return out


def _check_with_library(algorithm: str, params: dict[str, Any]) -> None:
    """Let scikit-learn's own parameter check catch what ranges cannot (cheap: nothing is fitted)."""
    cls = estimator_class(algorithm)
    try:
        est = cls(**params)
    except Exception as exc:  # noqa: BLE001 - constructor errors are user input errors here
        raise ValueError(f"invalid hyperparameters for {algorithm}: {exc}") from None
    check = getattr(est, "_validate_params", None)
    if check is None or not hasattr(cls, "_parameter_constraints"):
        return  # XGBoost checks its own parameters when it is fitted
    try:
        check()
    except Exception as exc:  # noqa: BLE001 - InvalidParameterError is a ValueError subclass
        raise ValueError(f"invalid hyperparameters for {algorithm}: {exc}") from None


def main() -> int:
    print(json.dumps(algorithms(), allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


# ------------------------------------------------------------------
# Model size guard
# ------------------------------------------------------------------

#: Largest model (and rough fit-time memory) a single training run may attempt.
MAX_MODEL_BYTES = 2 * 1024**3
#: Measured: a pickled tree node costs ~85 bytes, and a fully grown tree on noisy labels has about
#: 0.4 nodes per training row (pure leaves need many splits).
_BYTES_PER_NODE = 90
_NODES_PER_ROW = 0.4


def _as_int(value, default):
    try:
        return default if value is None else int(value)
    except (TypeError, ValueError):
        return default


def _tree_nodes(rows: int, max_depth, min_samples_leaf: int = 1, max_leaf_nodes=None) -> float:
    """Worst-case nodes in one tree on `rows` rows."""
    nodes = max(1.0, _NODES_PER_ROW * rows * 2.0 / (2.0 * max(1, min_samples_leaf)))
    if max_depth is not None and max_depth > 0:
        nodes = min(nodes, 2.0 ** (min(max_depth, 60) + 1))
    if max_leaf_nodes:
        nodes = min(nodes, 2.0 * max_leaf_nodes)
    return nodes


def estimate_model_bytes(algorithm: str, params: dict, rows: int, classes: int) -> int | None:
    """Worst-case size of the fitted model, or None for algorithms we cannot bound (custom code).

    Deliberately pessimistic: it assumes the trees grow as far as their limits allow.
    """
    classes = max(2, classes)
    per_round = 1 if classes == 2 else classes
    if algorithm in ("random_forest", "extra_trees"):
        trees = _as_int(params.get("n_estimators"), 100)
        depth = _as_int(params.get("max_depth"), 0) or None
        leaf = max(1, _as_int(params.get("min_samples_leaf"), 1))
        nodes = trees * _tree_nodes(rows, depth, leaf, _as_int(params.get("max_leaf_nodes"), 0) or None)
    elif algorithm == "gradient_boosting":
        rounds = _as_int(params.get("n_estimators"), 100)
        depth = _as_int(params.get("max_depth"), 3) or None
        nodes = rounds * per_round * _tree_nodes(rows, depth, 1, _as_int(params.get("max_leaf_nodes"), 0) or None)
    elif algorithm == "hist_gradient_boosting":
        rounds = _as_int(params.get("max_iter"), 100)
        depth = _as_int(params.get("max_depth"), 0) or None
        leaves = _as_int(params.get("max_leaf_nodes"), 31)
        nodes = rounds * per_round * _tree_nodes(rows, depth, 1, leaves)
    elif algorithm == "xgboost":
        rounds = _as_int(params.get("n_estimators"), 100)
        parallel = max(1, _as_int(params.get("num_parallel_tree"), 1))
        depth = _as_int(params.get("max_depth"), 6) or None
        nodes = rounds * parallel * per_round * _tree_nodes(rows, depth, 1, _as_int(params.get("max_leaves"), 0) or None)
    elif algorithm == "logistic_regression":
        return 8 * 1024 * 1024
    else:
        return None
    return int(nodes * _BYTES_PER_NODE)


def check_model_size(algorithm: str, params: dict, rows: int, classes: int) -> None:
    """Refuse a configuration whose model could not reasonably fit, before spending time on it."""
    size = estimate_model_bytes(algorithm, params, rows, classes)
    if size is not None and size > MAX_MODEL_BYTES:
        raise ValueError(
            f"this configuration could build a model of about {size / 1024**3:.1f} GB "
            f"(limit {MAX_MODEL_BYTES / 1024**3:.0f} GB) on {rows} rows. Lower the number of trees or rounds, "
            "set a max depth or max leaf nodes, or raise the minimum samples per leaf."
        )
