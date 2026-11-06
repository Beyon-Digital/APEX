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
