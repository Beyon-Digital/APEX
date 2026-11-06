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


# ------------------------------------------------------------------
# Algorithm recipes: building blocks (see ml.recipes)
# ------------------------------------------------------------------
#
# Everything below is a plain, module-level class so a pickled recipe model can be loaded by
# ``python -m ml.predict`` with nothing but ``ml.estimators`` importable.

from functools import partial  # noqa: E402

from sklearn.base import TransformerMixin  # noqa: E402
from sklearn.decomposition import PCA  # noqa: E402
from sklearn.feature_selection import SelectKBest, f_classif, mutual_info_classif  # noqa: E402
from sklearn.isotonic import IsotonicRegression  # noqa: E402
from sklearn.linear_model import LogisticRegression  # noqa: E402
from sklearn.utils.validation import check_is_fitted  # noqa: E402


class ClipOutliers(TransformerMixin, BaseEstimator):
    """Cap every feature at percentiles learned from the rows it is fitted on.

    ``fit`` stores the ``lower_pct`` and ``upper_pct`` percentile of each column of the training
    rows only; ``transform`` clips any data (training, test or live) to those stored bounds, so an
    extreme value that only appears later can never move the bounds.
    """

    def __init__(self, lower_pct: float = 1.0, upper_pct: float = 99.0):
        self.lower_pct = lower_pct
        self.upper_pct = upper_pct

    def fit(self, X, y=None):
        x = np.asarray(X, dtype=float)
        if x.ndim != 2:
            raise ValueError("expected a 2-D array of features")
        if not 0 <= self.lower_pct < self.upper_pct <= 100:
            raise ValueError("clip_outliers needs 0 <= lower_pct < upper_pct <= 100")
        bounds = np.nanpercentile(x, [self.lower_pct, self.upper_pct], axis=0)
        self.lower_, self.upper_ = bounds[0], bounds[1]
        self.n_features_in_ = x.shape[1]
        return self

    def transform(self, X):
        check_is_fitted(self, "lower_")
        x = np.asarray(X, dtype=float)
        if x.ndim != 2 or x.shape[1] != self.n_features_in_:
            raise ValueError(f"expected {self.n_features_in_} features, got {x.shape[1] if x.ndim == 2 else 'a non-2-D input'}")
        return np.clip(x, self.lower_, self.upper_)


class KBestSelector(TransformerMixin, BaseEstimator):
    """``SelectKBest`` whose ``k`` is capped to the number of features when it is fitted."""

    def __init__(self, k: int = 10, score: str = "f_classif", random_state: int | None = None):
        self.k = k
        self.score = score
        self.random_state = random_state

    def fit(self, X, y):
        x = np.asarray(X, dtype=float)
        if self.score == "mutual_info_classif":
            func = partial(mutual_info_classif, random_state=self.random_state)
        elif self.score == "f_classif":
            func = f_classif
        else:
            raise ValueError(f"unknown score '{self.score}' (use f_classif or mutual_info_classif)")
        self.k_ = int(max(1, min(int(self.k), x.shape[1])))
        self.selector_ = SelectKBest(func, k=self.k_).fit(x, y)
        return self

    def get_support(self, indices: bool = False):
        check_is_fitted(self, "selector_")
        return self.selector_.get_support(indices=indices)

    def transform(self, X):
        check_is_fitted(self, "selector_")
        return self.selector_.transform(np.asarray(X, dtype=float))


class PCAReducer(TransformerMixin, BaseEstimator):
    """PCA taking either a variance share (``n_components`` < 1) or a component count (>= 1).

    A count is capped to what the training rows allow (``min(rows, features)``).
    """

    def __init__(self, n_components: float = 0.95, whiten: bool = False, random_state: int | None = None):
        self.n_components = n_components
        self.whiten = whiten
        self.random_state = random_state

    def fit(self, X, y=None):
        x = np.asarray(X, dtype=float)
        n = float(self.n_components)
        if n < 1:
            pca = PCA(n_components=n, svd_solver="full", whiten=bool(self.whiten))
        else:
            pca = PCA(
                n_components=int(max(1, min(int(round(n)), x.shape[0], x.shape[1]))),
                whiten=bool(self.whiten), random_state=self.random_state,
            )
        self.pca_ = pca.fit(x)
        return self

    @property
    def n_components_(self) -> int:
        return int(self.pca_.n_components_)

    @property
    def explained_variance_ratio_(self):
        return self.pca_.explained_variance_ratio_

    def transform(self, X):
        check_is_fitted(self, "pca_")
        return self.pca_.transform(np.asarray(X, dtype=float))


class _ConstantClassifier(ClassifierMixin, BaseEstimator):
    """Stands in for a member when its training rows hold a single class."""

    def __init__(self, label=0):
        self.label = label

    def fit(self, x, y=None):
        self.classes_ = np.asarray([self.label])
        return self

    def predict(self, x):
        return np.full(len(x), self.label)

    def predict_proba(self, x):
        return np.ones((len(x), 1))


def _fit_member(estimator, x, y):
    """Fit ``estimator``; with a single class in ``y`` fall back to predicting that class."""
    classes = np.unique(y)
    if len(classes) < 2:
        return _ConstantClassifier(classes[0] if len(classes) else 0).fit(x, y)
    return estimator.fit(x, y)


def _aligned_proba(estimator, x, classes: np.ndarray) -> np.ndarray:
    """Class probabilities as an ``(rows, len(classes))`` array in the column order of ``classes``.

    * A member that was fitted without some class gets zeros for it (its columns are matched by
      label, not by position) and each row is renormalised; a row that ends up all zero becomes
      uniform.
    * A member without ``predict_proba`` contributes one-hot class votes.
    """
    x = np.asarray(x, dtype=float)
    out = np.zeros((len(x), len(classes)))
    if hasattr(estimator, "predict_proba"):
        proba = np.asarray(estimator.predict_proba(x), dtype=float)
        columns = np.searchsorted(classes, np.asarray(estimator.classes_))
        for source, target in enumerate(columns):
            if target < len(classes) and classes[target] == np.asarray(estimator.classes_)[source]:
                out[:, target] = proba[:, source]
    else:
        pred = np.asarray(estimator.predict(x))
        columns = np.searchsorted(classes, pred)
        valid = (columns < len(classes)) & (classes[np.minimum(columns, len(classes) - 1)] == pred)
        out[np.flatnonzero(valid), columns[valid]] = 1.0
    out = np.nan_to_num(out, nan=0.0, posinf=0.0, neginf=0.0)
    total = out.sum(axis=1, keepdims=True)
    empty = total[:, 0] <= 0
    out = np.divide(out, total, out=np.zeros_like(out), where=total > 0)
    out[empty] = 1.0 / len(classes)
    return out


#: Smallest number of rows the first stacking members may be trained on, and the smallest block.
MIN_STACK_TRAIN_ROWS = 20
MIN_STACK_BLOCK_ROWS = 5


def _final_has_proba(self) -> bool:
    return hasattr(self.final_estimator, "predict_proba")


class TimeAwareStacker(ClassifierMixin, BaseEstimator):
    """Stacking that never lets the future into a prediction (rows must be in time order).

    scikit-learn's ``StackingClassifier`` builds its meta-features with a partitioning cross
    validation, so a member predicting a row has been trained on rows from after it. This class
    does it the walk-forward way:

    1. The rows are cut into ``blocks + 1`` contiguous blocks (block 0 is the oldest).
    2. For ``j = 1 .. blocks``, a fresh clone of every member is fitted on rows ``[0, start_j -
       embargo_rows)`` and predicts block ``j``. ``embargo_rows`` is the reach of a label in rows
       (a label is decided up to ``horizon`` bars after its row), so no training label overlaps
       block ``j``. Block 0 is only ever training data and gets no meta-feature.
    3. The out-of-fold predictions for blocks ``1 .. blocks`` (class probabilities, or one-hot
       class votes for a member without ``predict_proba``) train ``final_estimator``.
    4. The members are refitted on all rows; ``predict`` feeds their predictions to the final
       estimator.

    ``keep_oof=True`` keeps the out-of-fold matrix as ``oof_`` (used by the tests).
    """

    def __init__(self, members, final_estimator, blocks: int = 5, embargo_rows: int = 0, keep_oof: bool = False):
        self.members = members
        self.final_estimator = final_estimator
        self.blocks = blocks
        self.embargo_rows = embargo_rows
        self.keep_oof = keep_oof

    def fit(self, X, y):
        x = np.asarray(X, dtype=float)
        y = np.asarray(y)
        n = len(y)
        blocks, embargo = int(self.blocks), max(int(self.embargo_rows), 0)
        if blocks < 2:
            raise ValueError("stacking needs at least 2 blocks")
        if not self.members:
            raise ValueError("stacking needs at least one member model")
        classes = np.unique(y)
        if len(classes) < 2:
            raise ValueError("stacking needs at least two classes in the training rows")
        bounds = np.linspace(0, n, blocks + 2).astype(int)
        first_train = int(bounds[1]) - embargo
        smallest_block = int(np.diff(bounds).min())
        if first_train < MIN_STACK_TRAIN_ROWS or smallest_block < MIN_STACK_BLOCK_ROWS:
            raise ValueError(
                f"not enough rows for stacking: {n} rows in {blocks + 1} blocks of about {n // (blocks + 1)}, with labels "
                f"reaching {embargo} rows ahead, leave {max(first_train, 0)} rows to train the first members on "
                f"(at least {MIN_STACK_TRAIN_ROWS} needed, and blocks of at least {MIN_STACK_BLOCK_ROWS}). "
                "Use fewer blocks, a shorter label horizon or more data."
            )

        folds = []
        for j in range(1, blocks + 1):
            lo, hi = int(bounds[j]), int(bounds[j + 1])
            end = lo - embargo  # rows [0, end) were decided before block j starts
            columns = [
                _aligned_proba(_fit_member(clone(est), x[:end], y[:end]), x[lo:hi], classes) for _, est in self.members
            ]
            folds.append(np.hstack(columns))
        meta_x = np.vstack(folds)
        meta_y = y[int(bounds[1]):]
        if len(np.unique(meta_y)) < 2:
            raise ValueError(
                "the rows used to train the final estimator hold only one class "
                f"({meta_y[0]}); use more data or fewer blocks"
            )
        self.classes_ = classes
        self.block_bounds_ = bounds
        self.final_ = _fit_member(clone(self.final_estimator), meta_x, meta_y)
        self.estimators_ = [(name, _fit_member(clone(est), x, y)) for name, est in self.members]
        if self.keep_oof:
            self.oof_ = meta_x
        return self

    def _meta(self, x) -> np.ndarray:
        x = np.asarray(x, dtype=float)
        return np.hstack([_aligned_proba(est, x, self.classes_) for _, est in self.estimators_])

    @available_if(_final_has_proba)
    def predict_proba(self, x):
        check_is_fitted(self, "final_")
        return _aligned_proba(self.final_, self._meta(x), self.classes_)

    def predict(self, x):
        check_is_fitted(self, "final_")
        meta = self._meta(x)
        if hasattr(self.final_, "predict_proba"):
            return self.classes_[np.argmax(_aligned_proba(self.final_, meta, self.classes_), axis=1)]
        return np.asarray(self.final_.predict(meta))


#: The calibrator needs at least this many rows to fit on, and the model at least this many.
MIN_CALIBRATION_ROWS = 30
MIN_CALIBRATED_MODEL_ROWS = 30


class TimeAwareCalibrated(ClassifierMixin, BaseEstimator):
    """Probability calibration that fits the calibrator on rows later than the model's.

    The rows are in time order. The model is fitted on the first part, the calibrator on the
    trailing ``calibration_fraction`` of the rows, and the ``embargo_rows`` before that slice are
    dropped so no model label reaches into it. (Cross-validated calibration would train the model
    on rows from after the ones it calibrates on.) The model keeps the earlier part only: refitting it on
    all rows would make its probabilities different from the ones the calibrator was fitted on.

    ``method`` is ``sigmoid`` (Platt scaling) or ``isotonic``, fitted per class one-vs-rest; the
    calibrated columns are renormalised. A class that is absent from (or fills) the calibration
    slice keeps its raw probability.
    """

    def __init__(self, estimator, method: str = "sigmoid", embargo_rows: int = 0, calibration_fraction: float = 0.2):
        self.estimator = estimator
        self.method = method
        self.embargo_rows = embargo_rows
        self.calibration_fraction = calibration_fraction

    def fit(self, X, y):
        x = np.asarray(X, dtype=float)
        y = np.asarray(y)
        n = len(y)
        if self.method not in ("sigmoid", "isotonic"):
            raise ValueError(f"calibration must be 'sigmoid' or 'isotonic', not {self.method!r}")
        n_cal = int(round(n * float(self.calibration_fraction)))
        embargo = max(int(self.embargo_rows), 0)
        model_rows = n - n_cal - embargo
        if n_cal < MIN_CALIBRATION_ROWS or model_rows < MIN_CALIBRATED_MODEL_ROWS:
            raise ValueError(
                f"not enough rows to calibrate: {n} rows leave {n_cal} for the calibrator and {max(model_rows, 0)} for the model "
                f"after the {embargo}-row label overlap (at least {MIN_CALIBRATION_ROWS} and {MIN_CALIBRATED_MODEL_ROWS} needed). "
                "Switch calibration off, use a shorter label horizon or more data."
            )
        cal_start = n - n_cal
        self.model_rows_ = int(model_rows)
        self.calibration_start_ = int(cal_start)
        self.estimator_ = _fit_member(clone(self.estimator), x[:model_rows], y[:model_rows])
        self.classes_ = np.asarray(self.estimator_.classes_)
        raw = np.asarray(self.estimator_.predict_proba(x[cal_start:]), dtype=float)
        y_cal = y[cal_start:]
        self.calibrators_ = []
        for column, label in enumerate(self.classes_):
            target = (y_cal == label).astype(float)
            if target.min() == target.max():  # class absent from (or filling) the slice
                self.calibrators_.append(None)
                continue
            self.calibrators_.append(self._fit_one(raw[:, column], target))
        return self

    def _fit_one(self, p: np.ndarray, t: np.ndarray):
        if self.method == "isotonic":
            return IsotonicRegression(y_min=0.0, y_max=1.0, out_of_bounds="clip").fit(p, t)
        return LogisticRegression(C=100.0).fit(self._logit(p).reshape(-1, 1), t.astype(int))

    @staticmethod
    def _logit(p: np.ndarray) -> np.ndarray:
        p = np.clip(p, 1e-6, 1 - 1e-6)
        return np.log(p / (1 - p))

    def predict_proba(self, x):
        check_is_fitted(self, "calibrators_")
        raw = np.asarray(self.estimator_.predict_proba(np.asarray(x, dtype=float)), dtype=float)
        out = raw.copy()
        for column, cal in enumerate(self.calibrators_):
            if cal is None:
                continue
            if self.method == "isotonic":
                out[:, column] = cal.predict(raw[:, column])
            else:
                out[:, column] = cal.predict_proba(self._logit(raw[:, column]).reshape(-1, 1))[:, 1]
        total = out.sum(axis=1, keepdims=True)
        empty = total[:, 0] <= 0
        out = np.divide(out, total, out=np.zeros_like(out), where=total > 0)
        out[empty] = 1.0 / out.shape[1]
        return out

    def predict(self, x):
        return self.classes_[np.argmax(self.predict_proba(x), axis=1)]
