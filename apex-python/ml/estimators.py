"""Scikit-learn helpers that must be importable when a saved model is loaded."""

from __future__ import annotations

import numpy as np
from sklearn.base import BaseEstimator, ClassifierMixin, clone
from sklearn.preprocessing import LabelEncoder


class LabelEncodedClassifier(ClassifierMixin, BaseEstimator):
    """Let a classifier that needs labels 0..k-1 (XGBoost) train on labels like -1/0/1.

    ``predict`` returns the original labels, so inference code never sees the encoding.
    """

    def __init__(self, estimator):
        self.estimator = estimator

    def fit(self, x, y):
        self.encoder_ = LabelEncoder().fit(y)
        self.classes_ = self.encoder_.classes_
        self.estimator_ = clone(self.estimator)
        self.estimator_.fit(x, self.encoder_.transform(y))
        return self

    def predict(self, x):
        return self.encoder_.inverse_transform(np.asarray(self.estimator_.predict(x)).astype(int))

    def predict_proba(self, x):
        return self.estimator_.predict_proba(x)

    @property
    def feature_importances_(self):
        return self.estimator_.feature_importances_


# ------------------------------------------------------------------
# Custom estimators
# ------------------------------------------------------------------
#
# A custom estimator is a Python file ``estimators/<name>.py`` that defines
#
#     def make_estimator(params: dict): ...
#
# and returns an object with ``fit(X, y)`` and ``predict(X)`` (scikit-learn style; ``predict_proba``
# and ``feature_importances_`` are used when present). The file is arbitrary local code, exactly
# like a strategy file. It is imported from its exact path, saved next to the model as
# ``<model_id>_estimator.py`` and imported again from that copy when the model is scored.

import hashlib
import importlib.util
import sys
from pathlib import Path

from sklearn.utils.metaestimators import available_if

ESTIMATOR_MODULE_PREFIX = "apex_estimator_"


def estimator_module_name(sha256: str) -> str:
    """Module name a custom estimator file is registered under (pickles refer to it by name)."""
    return f"{ESTIMATOR_MODULE_PREFIX}{sha256[:16]}"


def file_sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def import_estimator_file(path: str | Path, module_name: str):
    """Import exactly the file at ``path`` under ``module_name`` and return the module.

    Uses :mod:`importlib` with the file location, never the module search path, and writes no
    ``__pycache__`` next to the user's file.
    """
    path = Path(path)
    if not path.is_file():
        raise FileNotFoundError(f"estimator file not found: {path}")
    spec = importlib.util.spec_from_file_location(module_name, str(path))
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot import estimator file {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    previous = sys.dont_write_bytecode
    sys.dont_write_bytecode = True
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(module_name, None)
        raise
    finally:
        sys.dont_write_bytecode = previous
    return module


def _has_proba(self) -> bool:
    return hasattr(self.estimator, "predict_proba")


class CustomEstimator(ClassifierMixin, BaseEstimator):
    """Gives any ``make_estimator`` result the interface the trainer and predictor expect."""

    def __init__(self, estimator):
        self.estimator = estimator

    def fit(self, x, y):
        self.estimator.fit(x, y)
        self.classes_ = np.asarray(getattr(self.estimator, "classes_", np.unique(y)))
        return self

    def predict(self, x):
        return np.asarray(self.estimator.predict(x))

    @available_if(_has_proba)
    def predict_proba(self, x):
        return np.asarray(self.estimator.predict_proba(x))

    @property
    def feature_importances_(self):
        return self.estimator.feature_importances_  # AttributeError (so hasattr is False) if absent


def make_custom_estimator(module, params: dict) -> CustomEstimator:
    """Call the file's ``make_estimator(params)`` and check what it returned."""
    factory = getattr(module, "make_estimator", None)
    if not callable(factory):
        raise ValueError("the estimator file must define make_estimator(params)")
    inner = factory(dict(params))
    for method in ("fit", "predict"):
        if not callable(getattr(inner, method, None)):
            raise ValueError(f"make_estimator must return an object with a {method}() method (got {type(inner).__name__})")
    return CustomEstimator(inner)
