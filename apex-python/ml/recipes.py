"""Algorithm recipes: an algorithm assembled from library parts, with no code.

A recipe is declarative JSON (``RecipeDto``): up to eight preprocessing steps fitted on the
training rows, then one model, a voting ensemble or a time-aware stacking ensemble of built-in
algorithms, optionally wrapped in probability calibration. It only ever assembles components of
this package and scikit-learn, so training with one needs no approval.

* :func:`validate` checks a recipe deeply and returns a ``RecipeCheckDto``-shaped dict.
* :func:`blocks` describes the parts a recipe may use (``BuildingBlocksDto``).
* :func:`build` returns the unfitted estimator (a pipeline, wrapped in the calibrator on request).
* :func:`importances` reports feature importances for a fitted recipe model, best effort.

Command line::

    python -m ml.recipes blocks
    python -m ml.recipes validate --json '<RecipeDto JSON>'

Both print one JSON document. ``validate`` exits 0 even when the recipe is invalid (``ok`` is
false); it also never crashes: an unexpected error is reported as an issue.
"""

from __future__ import annotations

import argparse
import copy
import json
import math
import re
from typing import Any

import numpy as np
from sklearn.ensemble import VotingClassifier
from sklearn.pipeline import Pipeline
from sklearn.preprocessing import MinMaxScaler, PowerTransformer, QuantileTransformer, RobustScaler, StandardScaler

from ml import catalog
from ml.estimators import (
    ClipOutliers,
    KBestSelector,
    LabelEncodedClassifier,
    PCAReducer,
    TimeAwareCalibrated,
    TimeAwareStacker,
)

NAME_RE = re.compile(r"^[a-z][a-z0-9_]{0,40}$")
MAX_STEPS = 8
MIN_MEMBERS = 2
MAX_MEMBERS = 6
STACKING_BLOCKS = (3, 8)
MAX_WEIGHT = 100.0
CALIBRATIONS = ("none", "sigmoid", "isotonic")
MAX_LABEL = 80
MAX_DESCRIPTION = 1000
#: Dataset size the size estimate in a check assumes (the real run knows the real number).
NOMINAL_ROWS = 20_000
NOMINAL_CLASSES = 3
#: Estimates above this many MB are flagged in the check, long before the hard limit.
SIZE_WARN_MB = 512


# ------------------------------------------------------------------
# Building blocks
# ------------------------------------------------------------------


def _p(key: str, label: str, type_: str, default: Any, help_: str, **extra: Any) -> dict[str, Any]:
    out: dict[str, Any] = {"key": key, "label": label, "type": type_, "default": default}
    for k in ("min", "max", "choices"):
        if k in extra:
            out[k] = extra[k]
    out["help"] = help_
    return out


_STEPS: list[dict[str, Any]] = [
    {
        "kind": "standard_scaler", "label": "Standard scaler", "group": "scale",
        "description": "Centre each feature and divide by its spread.",
        "params": [
            _p("with_mean", "Centre", "bool", True, "Subtract the mean."),
            _p("with_std", "Scale", "bool", True, "Divide by the standard deviation."),
        ],
    },
    {
        "kind": "robust_scaler", "label": "Robust scaler", "group": "scale",
        "description": "Scale by the middle of the data, ignoring extreme values.", "params": [],
    },
    {
        "kind": "minmax_scaler", "label": "Min-max scaler", "group": "scale",
        "description": "Squeeze each feature into 0..1.", "params": [],
    },
    {
        "kind": "quantile_transformer", "label": "Quantile transformer", "group": "scale",
        "description": "Turn each feature into its rank, spread evenly or like a bell curve.",
        "params": [
            _p("output_distribution", "Output", "choice", "uniform", "Shape of the result.", choices=["uniform", "normal"]),
            _p("n_quantiles", "Quantiles", "int", 200, "Resolution of the rank table.", min=10, max=2000),
        ],
    },
    {
        "kind": "power_transformer", "label": "Power transformer", "group": "scale",
        "description": "Make skewed features more bell-shaped (Yeo-Johnson).", "params": [],
    },
    {
        "kind": "clip_outliers", "label": "Clip outliers", "group": "clean",
        "description": "Cap each feature at chosen percentiles learned from the training rows.",
        "params": [
            _p("lower_pct", "Lower percentile", "float", 1, "Values below this percentile are raised to it.", min=0, max=20),
            _p("upper_pct", "Upper percentile", "float", 99, "Values above this percentile are lowered to it.", min=80, max=100),
        ],
    },
    {
        "kind": "variance_threshold", "label": "Drop flat features", "group": "select",
        "description": "Remove features that barely change.",
        "params": [_p("threshold", "Min variance", "float", 0, "Features with less variance are dropped.", min=0, max=100)],
    },
    {
        "kind": "select_k_best", "label": "Keep the best k features", "group": "select",
        "description": "Keep the k features most related to the label.",
        "params": [
            _p("k", "k", "int", 10, "How many to keep (at most the number of features).", min=1, max=500),
            _p("score", "Score", "choice", "f_classif", "How features are ranked.", choices=["f_classif", "mutual_info_classif"]),
        ],
    },
    {
        "kind": "pca", "label": "PCA", "group": "reduce",
        "description": "Replace features with a few uncorrelated combinations.",
        "params": [
            _p("n_components", "Components", "float", 0.95,
               "Under 1: share of variance to keep. 1 or more: number of components (a whole number).", min=0.1, max=200),
            _p("whiten", "Whiten", "bool", False, "Give every component equal spread."),
        ],
    },
]
_STEP_BY_KIND = {s["kind"]: s for s in _STEPS}


def member_ids() -> list[str]:
    """Built-in algorithms that may be a member or the final estimator (xgboost only if importable)."""
    return [a for a in catalog.supported_ids() if a != catalog.CUSTOM]


def blocks() -> dict[str, Any]:
    """The parts a recipe may use, as ``BuildingBlocksDto``."""
    members = []
    for algorithm in member_ids():
        info = catalog._ALGORITHMS[algorithm]  # noqa: SLF001 - the catalog owns the labels
        members.append({"id": algorithm, "label": info["label"], "description": info["description"]})
    return {
        "steps": copy.deepcopy(_STEPS),
        "members": members,
        "limits": {
            "max_steps": MAX_STEPS, "max_members": MAX_MEMBERS, "min_members": MIN_MEMBERS,
            "stacking_blocks": list(STACKING_BLOCKS),
        },
    }


# ------------------------------------------------------------------
# Validation
# ------------------------------------------------------------------


def _is_number(v: Any) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def _is_int(v: Any) -> bool:
    return _is_number(v) and float(v).is_integer()


class _Report:
    def __init__(self) -> None:
        self.errors: list[dict[str, str]] = []
        self.warnings: list[str] = []

    def bad(self, path: str, message: str) -> None:
        self.errors.append({"path": path, "message": message})

    def warn(self, message: str) -> None:
        if message not in self.warnings:
            self.warnings.append(message)


def _check_step_params(kind: str, params: Any, path: str, rep: _Report) -> dict[str, Any]:
    """Validate a step's parameters against the table; return them with defaults filled in."""
    spec = _STEP_BY_KIND[kind]
    defs = {d["key"]: d for d in spec["params"]}
    if params is None:
        params = {}
    if not isinstance(params, dict):
        rep.bad(f"{path}.params", "parameters must be an object")
        return {d["key"]: d["default"] for d in spec["params"]}
    out: dict[str, Any] = {}
    for key, value in params.items():
        where = f"{path}.params.{key}"
        d = defs.get(key)
        if d is None:
            valid = ", ".join(defs) if defs else "none"
            rep.bad(where, f"unknown parameter '{key}' for {kind} (valid: {valid})")
            continue
        t, lo, hi = d["type"], d.get("min"), d.get("max")
        if t == "bool":
            if isinstance(value, bool):
                out[key] = value
            else:
                rep.bad(where, "expected true or false")
        elif t == "choice":
            if isinstance(value, str) and value in d["choices"]:
                out[key] = value
            else:
                rep.bad(where, "expected one of " + ", ".join(repr(c) for c in d["choices"]))
        elif t in ("int", "float"):
            ok = _is_int(value) if t == "int" else _is_number(value)
            if not ok or (lo is not None and value < lo) or (hi is not None and value > hi):
                kind_word = "an integer" if t == "int" else "a number"
                rep.bad(where, f"expected {kind_word} from {lo:g} to {hi:g}")
            else:
                out[key] = int(value) if t == "int" else float(value)
    for key, d in defs.items():
        out.setdefault(key, d["default"])
    if kind == "clip_outliers" and not any(e["path"].startswith(f"{path}.params.") for e in rep.errors):
        if out["lower_pct"] >= out["upper_pct"]:
            rep.bad(f"{path}.params.upper_pct", "the upper percentile must be above the lower percentile")
    if kind == "pca" and "n_components" in params and not any(e["path"] == f"{path}.params.n_components" for e in rep.errors):
        n = out["n_components"]
        if n >= 1 and not float(n).is_integer():
            rep.bad(f"{path}.params.n_components", "1 or more means a number of components, so it must be a whole number")
    return out


def _check_member(raw: Any, path: str, rep: _Report, *, weight: bool = False) -> dict[str, Any] | None:
    """Validate one member (algorithm plus parameters); return it normalised, or None if unusable."""
    if not isinstance(raw, dict):
        rep.bad(path, "expected an object with an algorithm and params")
        return None
    algorithm = raw.get("algorithm")
    allowed = member_ids()
    if not isinstance(algorithm, str) or algorithm not in allowed:
        rep.bad(f"{path}.algorithm", f"unknown algorithm {algorithm!r} (choose from {', '.join(allowed)})")
        return None
    params = raw.get("params")
    if params is None:
        params = {}
    ok = True
    clean: dict[str, Any] = {}
    if not isinstance(params, dict):
        rep.bad(f"{path}.params", "parameters must be an object")
        ok = False
    else:
        for key, value in params.items():
            try:
                clean.update(catalog.validate_hyperparams(algorithm, {key: value}))
            except ValueError as exc:
                rep.bad(f"{path}.params.{key}", str(exc))
                ok = False
        if ok:
            try:  # cross-parameter checks that need the whole set
                clean = catalog.validate_hyperparams(algorithm, clean)
            except ValueError as exc:
                rep.bad(f"{path}.params", str(exc))
                ok = False
    out: dict[str, Any] = {"algorithm": algorithm, "params": clean if ok else {}}
    if weight:
        w = raw.get("weight", 1)
        if w is None:
            w = 1
        if not _is_number(w) or not 0 < w <= MAX_WEIGHT:
            rep.bad(f"{path}.weight", f"expected a number above 0 and at most {MAX_WEIGHT:g}")
        else:
            out["weight"] = float(w)
    return out if ok else None


def _check_model(raw: Any, rep: _Report) -> tuple[dict[str, Any] | None, int, list[tuple[str, dict[str, Any]]]]:
    """Validate the model; return (normalised, fitted model count, valid members for size estimates)."""
    if not isinstance(raw, dict):
        rep.bad("model", "choose a model")
        return None, 0, []
    kind = raw.get("kind")
    sized: list[tuple[str, dict[str, Any]]] = []
    if kind == "single":
        m = _check_member(raw, "model", rep)
        if m:
            sized.append(("model", m))
        out = {"kind": "single", "algorithm": raw.get("algorithm"), "params": (m or {}).get("params", {})}
        return out if m else None, 1, sized
    if kind not in ("voting", "stacking"):
        rep.bad("model.kind", "choose single, voting or stacking")
        return None, 0, []

    out: dict[str, Any] = {"kind": kind}
    members_raw = raw.get("members")
    members: list[dict[str, Any]] = []
    count = 0
    if not isinstance(members_raw, list):
        rep.bad("model.members", f"choose {MIN_MEMBERS} to {MAX_MEMBERS} models")
    else:
        count = len(members_raw)
        if not MIN_MEMBERS <= count <= MAX_MEMBERS:
            rep.bad("model.members", f"use {MIN_MEMBERS} to {MAX_MEMBERS} models (got {count})")
        for i, item in enumerate(members_raw[:MAX_MEMBERS + 1]):
            m = _check_member(item, f"model.members[{i}]", rep, weight=kind == "voting")
            if m:
                members.append(m)
                sized.append((f"model.members[{i}]", m))
    complete = isinstance(members_raw, list) and len(members) == count and MIN_MEMBERS <= count <= MAX_MEMBERS
    out["members"] = members
    if kind == "voting":
        voting = raw.get("voting", "soft")
        if voting not in ("soft", "hard"):
            rep.bad("model.voting", "choose soft or hard")
        out["voting"] = voting
    else:
        count += 1
        final = _check_member(raw.get("final"), "model.final", rep)
        if final:
            sized.append(("model.final", final))
        out["final"] = final
        blocks_ = raw.get("blocks", 5)
        lo, hi = STACKING_BLOCKS
        if not _is_int(blocks_) or not lo <= blocks_ <= hi:
            rep.bad("model.blocks", f"use {lo} to {hi} blocks")
        else:
            out["blocks"] = int(blocks_)
        complete = complete and final is not None and "blocks" in out
    complete = complete and not any(e["path"].startswith("model") for e in rep.errors)
    return (out if complete else None), count, sized


def _estimate_bytes(sized: list[tuple[str, dict[str, Any]]], rows: int, classes: int) -> list[tuple[str, str, int | None]]:
    return [(path, m["algorithm"], catalog.estimate_model_bytes(m["algorithm"], m["params"], rows, classes)) for path, m in sized]


def _check(recipe: Any, *, identity: bool = True) -> dict[str, Any]:
    """Collect every problem. Returns the check dict plus the normalised recipe under ``_recipe``."""
    rep = _Report()
    if not isinstance(recipe, dict):
        rep.bad("", "recipe is required and must be an object")
        return {"ok": False, "errors": rep.errors, "warnings": [], "steps": 0, "models": 0, "est_model_mb": None, "_recipe": None}

    name, label, description = recipe.get("name"), recipe.get("label"), recipe.get("description", "")
    if identity:
        if not isinstance(name, str) or not NAME_RE.match(name):
            rep.bad("name", "use lowercase letters, digits and underscores, starting with a letter (max 41 characters)")
        if not isinstance(label, str) or not label.strip():
            rep.bad("label", "give the algorithm a name")
    if isinstance(label, str) and len(label) > MAX_LABEL:
        rep.bad("label", f"at most {MAX_LABEL} characters")
    if description is not None and not isinstance(description, str):
        rep.bad("description", "expected text")
    elif isinstance(description, str) and len(description) > MAX_DESCRIPTION:
        rep.bad("description", f"at most {MAX_DESCRIPTION} characters")

    steps_raw = recipe.get("steps", [])
    steps: list[dict[str, Any]] = []
    if steps_raw is None:
        steps_raw = []
    if not isinstance(steps_raw, list):
        rep.bad("steps", "expected a list of steps")
        steps_raw = []
    if len(steps_raw) > MAX_STEPS:
        rep.bad("steps", f"at most {MAX_STEPS} steps (got {len(steps_raw)})")
    for i, item in enumerate(steps_raw[:MAX_STEPS + 1]):
        path = f"steps[{i}]"
        if not isinstance(item, dict):
            rep.bad(path, "expected an object with a kind and params")
            continue
        kind = item.get("kind")
        if not isinstance(kind, str) or kind not in _STEP_BY_KIND:
            rep.bad(f"{path}.kind", f"unknown step {kind!r} (choose from {', '.join(_STEP_BY_KIND)})")
            continue
        steps.append({"kind": kind, "params": _check_step_params(kind, item.get("params"), path, rep)})

    calibrate = recipe.get("calibrate", "none")
    if calibrate is None:
        calibrate = "none"
    if calibrate not in CALIBRATIONS:
        rep.bad("calibrate", "choose none, sigmoid or isotonic")

    model, models, sized = _check_model(recipe.get("model"), rep)

    # Warnings: things that are legal but surprising.
    kinds = {s["kind"] for s in steps}
    if "pca" in kinds and models > 0:
        rep.warn("PCA hides the original features, so feature importances are not available for this model.")
    if model and model["kind"] == "voting" and model.get("voting") == "hard":
        if calibrate in ("sigmoid", "isotonic"):
            rep.warn("Hard voting has no probabilities to calibrate; the calibration is ignored.")
        rep.warn("Hard voting gives class votes only, so this model reports no probability (confidence).")
    if model and model["kind"] == "stacking":
        rep.warn(
            f"Stacking keeps the oldest 1/{model['blocks'] + 1} of the training rows for the first members only: "
            "the final estimator learns from the later rows."
        )
    algorithms = {m["algorithm"] for _, m in sized}
    if "logistic_regression" in algorithms and not kinds & {"standard_scaler", "robust_scaler", "minmax_scaler", "quantile_transformer", "power_transformer"}:
        rep.warn("Logistic regression works best on scaled features; add a scaler step.")
    if "select_k_best" in kinds and "variance_threshold" in kinds:
        order = [s["kind"] for s in steps]
        if order.index("select_k_best") < order.index("variance_threshold"):
            rep.warn("Dropping flat features after choosing the best k can leave fewer than k features.")
    if calibrate in ("sigmoid", "isotonic") and model and not (model["kind"] == "voting" and model.get("voting") == "hard"):
        rep.warn("Calibration trains the model on the earlier rows and the calibrator on the latest 20% of them.")

    est_mb = None
    sizes = _estimate_bytes(sized, NOMINAL_ROWS, NOMINAL_CLASSES)
    if sizes and all(s is not None for _, _, s in sizes) and model is not None:
        est_mb = round(sum(s for _, _, s in sizes) / 1024**2, 1)
        if est_mb > SIZE_WARN_MB:
            rep.warn(
                f"This recipe could build about {est_mb:g} MB of models on {NOMINAL_ROWS} rows; "
                "training refuses it above the size limit. Lower trees or rounds, or set a max depth."
            )

    normalised = None
    if not rep.errors and model is not None:
        normalised = {
            "name": name if isinstance(name, str) else "",
            "label": label.strip() if isinstance(label, str) else "",
            "description": description or "",
            "steps": steps,
            "model": model,
            "calibrate": calibrate,
        }
        for key in ("created_at", "updated_at"):
            if isinstance(recipe.get(key), str):
                normalised[key] = recipe[key]
    return {
        "ok": not rep.errors,
        "errors": rep.errors,
        "warnings": rep.warnings,
        "steps": len(steps_raw),
        "models": models,
        "est_model_mb": est_mb,
        "_recipe": normalised,
    }


def validate(recipe: Any) -> dict[str, Any]:
    """Check a recipe deeply and return ``RecipeCheckDto``: every issue with its path, never raising."""
    try:
        check = _check(recipe)
    except Exception as exc:  # noqa: BLE001 - a malformed recipe must be reported, not crash the caller
        return {"ok": False, "errors": [{"path": "", "message": f"could not check the recipe: {exc}"}],
                "warnings": [], "steps": 0, "models": 0, "est_model_mb": None}
    check.pop("_recipe")
    return check


def prepare(recipe: Any, *, identity: bool = False) -> dict[str, Any]:
    """Validate and return the normalised recipe (defaults filled, values coerced), or raise ``ValueError``.

    The trainer uses this with ``identity=False`` so an unsaved draft without a name still trains.
    """
    check = _check(recipe, identity=identity)
    if not check["ok"] or check["_recipe"] is None:
        shown = "; ".join(f"{e['path'] or 'recipe'}: {e['message']}" for e in check["errors"][:6])
        more = len(check["errors"]) - 6
        raise ValueError(f"invalid recipe: {shown}" + (f" (and {more} more)" if more > 0 else ""))
    return check["_recipe"]


def check_model_size(recipe: dict[str, Any], rows: int, classes: int) -> None:
    """Refuse a recipe whose models together could not reasonably fit, before spending time on it.

    ``recipe`` must be normalised (see :func:`prepare`). Every member and the final estimator is
    held in the saved model, so the estimates are summed.
    """
    model = recipe["model"]
    parts: list[tuple[str, dict[str, Any]]]
    if model["kind"] == "single":
        parts = [("model", model)]
    else:
        parts = [(f"member {i + 1}", m) for i, m in enumerate(model["members"])]
        if model["kind"] == "stacking":
            parts.append(("the final estimator", model["final"]))
    sizes = _estimate_bytes(parts, rows, classes)
    total = sum(s for _, _, s in sizes if s is not None)
    if total > catalog.MAX_MODEL_BYTES:
        worst = max(sizes, key=lambda t: t[2] or 0)
        raise ValueError(
            f"this recipe could build models totalling about {total / 1024**3:.1f} GB "
            f"(limit {catalog.MAX_MODEL_BYTES / 1024**3:.0f} GB) on {rows} rows; the largest is {worst[0]} "
            f"({worst[1]}, about {(worst[2] or 0) / 1024**3:.1f} GB). Lower the number of trees or rounds, "
            "set a max depth or max leaf nodes, raise the minimum samples per leaf, or use fewer models."
        )


# ------------------------------------------------------------------
# Building
# ------------------------------------------------------------------


def _make_step(kind: str, params: dict[str, Any], seed: int):
    if kind == "standard_scaler":
        return StandardScaler(with_mean=params["with_mean"], with_std=params["with_std"])
    if kind == "robust_scaler":
        return RobustScaler()
    if kind == "minmax_scaler":
        return MinMaxScaler()
    if kind == "quantile_transformer":
        return QuantileTransformer(
            output_distribution=params["output_distribution"], n_quantiles=params["n_quantiles"], random_state=seed
        )
    if kind == "power_transformer":
        return PowerTransformer(method="yeo-johnson")
    if kind == "clip_outliers":
        return ClipOutliers(lower_pct=params["lower_pct"], upper_pct=params["upper_pct"])
    if kind == "variance_threshold":
        from sklearn.feature_selection import VarianceThreshold  # noqa: PLC0415

        return VarianceThreshold(threshold=params["threshold"])
    if kind == "select_k_best":
        return KBestSelector(k=params["k"], score=params["score"], random_state=seed)
    if kind == "pca":
        return PCAReducer(n_components=params["n_components"], whiten=params["whiten"], random_state=seed)
    raise ValueError(f"unknown step {kind}")


def _make_member(algorithm: str, params: dict[str, Any], seed: int):
    cls = catalog.estimator_class(algorithm)
    kwargs = dict(params)
    if "random_state" in catalog.param_specs(algorithm):
        kwargs.setdefault("random_state", seed)
    if algorithm == "xgboost":
        kwargs.setdefault("n_jobs", 1)
        kwargs.setdefault("verbosity", 0)
        # Class labels are -1/0/1 or 0/1 and can lose a class inside a fold; XGBoost needs 0..k-1.
        return LabelEncodedClassifier(cls(**kwargs))
    return cls(**kwargs)


def build(recipe: Any, *, embargo_rows: int = 0, random_state: int | None = None):
    """Return an unfitted estimator for ``recipe``.

    ``embargo_rows`` is how many rows ahead a training label reaches (the label horizon in rows).
    The time-aware stacker and calibrator use it to keep every label that overlaps the rows they
    predict out of their training data. The result is a :class:`~sklearn.pipeline.Pipeline` of the
    steps and the model, wrapped in :class:`~ml.estimators.TimeAwareCalibrated` when asked for
    (so the preprocessing is fitted on the model's rows only). Hard voting has no probabilities, so
    its calibration is dropped.
    """
    spec = prepare(recipe)
    seed = catalog.DEFAULT_SEED if random_state is None else int(random_state)
    embargo = max(int(embargo_rows), 0)
    model = spec["model"]

    def make_steps() -> list[tuple[str, Any]]:
        return [(f"s{i}_{s['kind']}", _make_step(s["kind"], s["params"], seed)) for i, s in enumerate(spec["steps"])]

    if model["kind"] == "stacking":
        # The steps go INSIDE every stacker member. The stacker fits a member on the rows before each
        # block, so the steps (select_k_best uses the labels; scalers, PCA and clipping use the feature
        # distribution) are fitted on those rows only. Fitting them once on all rows first would let block
        # j's labels and features choose the columns its own out-of-fold predictions are made from.
        members = [
            (f"m{i}_{m['algorithm']}", Pipeline([*make_steps(), ("model", _make_member(m["algorithm"], m["params"], seed))]))
            for i, m in enumerate(model["members"])
        ]
        meta = model["final"]
        pipeline = TimeAwareStacker(
            members, _make_member(meta["algorithm"], meta["params"], seed), blocks=model["blocks"], embargo_rows=embargo
        )
    else:
        if model["kind"] == "single":
            final = _make_member(model["algorithm"], model["params"], seed)
        else:
            estimators = [(f"m{i}_{m['algorithm']}", _make_member(m["algorithm"], m["params"], seed)) for i, m in enumerate(model["members"])]
            final = VotingClassifier(estimators, voting=model["voting"], weights=[m.get("weight", 1.0) for m in model["members"]])
        pipeline = Pipeline([*make_steps(), ("model", final)])
    hard = model["kind"] == "voting" and model.get("voting") == "hard"
    if spec["calibrate"] != "none" and not hard:
        return TimeAwareCalibrated(pipeline, method=spec["calibrate"], embargo_rows=embargo)
    return pipeline


# ------------------------------------------------------------------
# Feature importances
# ------------------------------------------------------------------


def _single_importance(est: Any, d: int, scale: np.ndarray | None) -> np.ndarray | None:
    """Normalised importances of one fitted estimator over ``d`` features, or None.

    Tree importances are scale free. A linear model's coefficient depends on the unit of its feature,
    so it is reported as the standardised coefficient ``|coef| * std(feature)`` using ``scale`` (the
    standard deviation of the columns the model was fitted on). Without ``scale`` it reports none,
    rather than let a tiny-scale feature look important.
    """
    try:
        if isinstance(est, LabelEncodedClassifier):
            est = est.estimator_
        if hasattr(est, "feature_importances_"):
            raw = np.asarray(est.feature_importances_, dtype=float)
        elif hasattr(est, "coef_"):
            if scale is None or np.shape(scale) != (d,):
                return None
            raw = np.abs(np.asarray(est.coef_, dtype=float)).reshape(-1, d).mean(axis=0) * np.asarray(scale, dtype=float)
        else:
            return None
    except Exception:  # noqa: BLE001 - importances are optional
        return None
    if raw.shape != (d,) or not np.isfinite(raw).all() or raw.sum() <= 0:
        return None
    return raw / raw.sum()


def _model_importance(model: Any, d: int, scale: np.ndarray | None) -> np.ndarray | None:
    if isinstance(model, VotingClassifier):
        weights = model.weights if model.weights is not None else [1.0] * len(model.estimators_)
        parts = [(w, _single_importance(e, d, scale)) for w, e in zip(weights, model.estimators_)]
    else:
        return _single_importance(model, d, scale)
    parts = [(w, p) for w, p in parts if p is not None]
    if not parts:
        return None
    total = sum(w for w, _ in parts)
    return sum(w * p for w, p in parts) / total


def _pipeline_importance(pipe: Pipeline, n_features: int, x: np.ndarray | None) -> tuple[np.ndarray, np.ndarray] | None:
    """Importance of each of the ``n_features`` input columns of a fitted pipeline, and which columns survive its steps."""
    kept = np.arange(n_features)
    xt = None if x is None else np.asarray(x, dtype=float)
    for _, step in pipe.steps[:-1]:
        if isinstance(step, PCAReducer):
            return None
        if hasattr(step, "get_support"):
            mask = np.asarray(step.get_support(), dtype=bool)
            if mask.shape != (len(kept),):
                return None
            kept = kept[mask]
        if xt is not None:
            xt = step.transform(xt)
    scale = None if xt is None else np.nan_to_num(np.std(xt, axis=0))
    raw = _model_importance(pipe.steps[-1][1], len(kept), scale)
    if raw is None:
        return None
    out = np.zeros(n_features)
    out[kept] = raw
    survives = np.zeros(n_features, dtype=bool)
    survives[kept] = True
    return out, survives


def importances(model: Any, names: list[str], x: Any = None) -> list[dict[str, Any]]:
    """Feature importances of a fitted recipe model, best effort (an empty list when unavailable).

    Reported when no step changes the feature set, or only ``variance_threshold`` and
    ``select_k_best`` do (the surviving features are found with ``get_support``). Members'
    importances are averaged (by weight for voting, equally for stacking). PCA hides the features,
    so it reports none. ``x`` is the training matrix: it gives the feature scales that turn a
    logistic regression's coefficients into comparable standardised ones. Without it, logistic
    members are left out of the average.
    """
    try:
        est = model.estimator_ if isinstance(model, TimeAwareCalibrated) else model
        n = len(names)
        xa = None if x is None else np.asarray(x, dtype=float)
        if isinstance(est, TimeAwareStacker):
            found = [_pipeline_importance(p, n, xa) for _, p in est.estimators_ if isinstance(p, Pipeline)]
        elif isinstance(est, Pipeline):
            found = [_pipeline_importance(est, n, xa)]
        else:
            return []
        found = [f for f in found if f is not None]
        if not found:
            return []
        raw = sum(f[0] for f in found) / len(found)
        survives = np.any([f[1] for f in found], axis=0)
        if raw.sum() <= 0:
            return []
        raw = raw / raw.sum()
        ranked = sorted(((n_, v) for n_, v, keep in zip(names, raw, survives) if keep), key=lambda kv: -kv[1])
        return [{"feature": f, "importance": float(v)} for f, v in ranked]
    except Exception:  # noqa: BLE001 - never let an optional report fail a training run
        return []


# ------------------------------------------------------------------
# Command line
# ------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Algorithm recipes")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("blocks", help="print the building blocks (BuildingBlocksDto)")
    check = sub.add_parser("validate", help="check a recipe (RecipeCheckDto)")
    check.add_argument("--json", required=True, dest="raw", help="the RecipeDto as JSON")
    args = parser.parse_args(argv)

    if args.command == "blocks":
        print(json.dumps(blocks(), allow_nan=False))
        return 0
    try:
        result = validate(json.loads(args.raw))
    except Exception as exc:  # noqa: BLE001 - always answer with a RecipeCheckDto
        result = {"ok": False, "errors": [{"path": "", "message": f"could not read the recipe: {exc}"}],
                  "warnings": [], "steps": 0, "models": 0, "est_model_mb": None}
    print(json.dumps(result, allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
