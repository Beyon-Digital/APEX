"""Custom estimators: trained from a user file, copied next to the model, scored from the copy."""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest

from ml.trainer import ModelTrainer, _config_from_json
from tests.test_trainer import FEATURES, make_config

APEX_PYTHON = Path(__file__).resolve().parents[1]

NEAREST_CENTROID = '''
"""Tiny custom estimator for the tests."""
import numpy as np


class Centroids:
    def __init__(self, params):
        self.shrink = float(params.get("shrink", 0.0))
        self.seed = params.get("random_state")

    def fit(self, X, y):
        X = np.asarray(X, dtype=float)
        self.classes_ = np.unique(y)
        self.centers_ = np.array([X[np.asarray(y) == c].mean(axis=0) for c in self.classes_]) * (1 - self.shrink)
        self.scale_ = X.std(axis=0) + 1e-9
        return self

    def _dist(self, X):
        X = np.asarray(X, dtype=float)
        return np.stack([(((X - c) / self.scale_) ** 2).sum(axis=1) for c in self.centers_], axis=1)

    def predict(self, X):
        return self.classes_[self._dist(X).argmin(axis=1)]

    def predict_proba(self, X):
        w = np.exp(-self._dist(X))
        return w / w.sum(axis=1, keepdims=True)


def make_estimator(params):
    return Centroids(params)
'''

NO_PROBA = NEAREST_CENTROID.replace("    def predict_proba", "    def _unused_proba")


def write_estimator(tmp_path: Path, text: str = NEAREST_CENTROID, name: str = "centroids.py") -> Path:
    folder = tmp_path / "estimators"
    folder.mkdir(exist_ok=True)
    path = folder / name
    path.write_text(text)
    return path


def custom_config(tmp_path: Path, path: Path, **overrides):
    base = dict(
        algorithm="custom", hyperparams={"shrink": 0.1}, estimator_file=path.name, estimator_path=str(path), n_splits=2,
    )
    base.update(overrides)
    return make_config(tmp_path, **base)


def predict_cli(model_path: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, "-m", "ml.predict", "--model-path", str(model_path), *args],
        cwd=APEX_PYTHON, capture_output=True, text=True,
    )


ROW = {"ret_1": 0.01, "rsi_14": 60.0, "vol_10": 0.01}


def test_a_custom_estimator_trains_is_copied_and_recorded(tmp_path):
    source = write_estimator(tmp_path)
    result = ModelTrainer().train(custom_config(tmp_path, source))
    meta = json.loads(result.metadata_path.read_text())

    out = result.model_path.parent
    copy = out / f"{result.model_path.stem}_estimator.py"
    assert copy.is_file() and copy.read_text() == source.read_text()
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    assert meta["estimator_file"] == "centroids.py" and meta["estimator_sha256"] == digest
    assert meta["estimator_copy"] == copy.name and meta["estimator_path"] == str(source)
    assert meta["estimator_module"].startswith("apex_estimator_")
    assert meta["algorithm"] == "custom"
    # The free-form parameters reached make_estimator, together with the fixed seed.
    assert meta["hyperparams"] == {"shrink": 0.1, "random_state": 42}
    assert 0 <= meta["metrics"]["accuracy"] <= 1 and len(meta["cv_folds"]) == 2
    assert not (source.parent / "__pycache__").exists(), "importing must not write bytecode next to the user's file"


def test_scoring_uses_the_copy_after_the_original_is_edited_or_deleted(tmp_path):
    source = write_estimator(tmp_path)
    result = ModelTrainer().train(custom_config(tmp_path, source))
    before = predict_cli(result.model_path, "--features-json", json.dumps(ROW))
    assert before.returncode == 0, before.stderr
    expected = json.loads(before.stdout.strip().splitlines()[-1])
    assert expected["signal"] in (0, 1) and 0 < expected["probability"] <= 1

    # Edit the original so make_estimator would now return something else, then delete it.
    source.write_text("def make_estimator(params):\n    raise RuntimeError('edited')\n")
    still = predict_cli(result.model_path, "--features-json", json.dumps(ROW))
    assert still.returncode == 0, still.stderr
    assert json.loads(still.stdout.strip().splitlines()[-1]) == expected
    source.unlink()
    rows = [[0.01, 60.0, 0.01], [-0.02, 30.0, 0.02]]
    batch = predict_cli(result.model_path, "--features-matrix-json", json.dumps(rows))
    assert batch.returncode == 0, batch.stderr
    assert len(json.loads(batch.stdout.strip().splitlines()[-1])["signals"]) == 2


def test_scoring_fails_clearly_if_the_models_copy_is_gone(tmp_path):
    source = write_estimator(tmp_path)
    result = ModelTrainer().train(custom_config(tmp_path, source))
    (result.model_path.parent / f"{result.model_path.stem}_estimator.py").unlink()
    out = predict_cli(result.model_path, "--features-json", json.dumps(ROW))
    assert out.returncode != 0 and "estimator file is missing" in out.stderr


def test_an_estimator_without_predict_proba_scores_with_no_probability(tmp_path):
    source = write_estimator(tmp_path, NO_PROBA, "plain.py")
    result = ModelTrainer().train(custom_config(tmp_path, source))
    out = predict_cli(result.model_path, "--features-json", json.dumps(ROW))
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout.strip().splitlines()[-1])["probability"] is None


def test_the_same_seed_gives_the_same_custom_model(tmp_path):
    source = write_estimator(tmp_path)
    a = ModelTrainer().train(custom_config(tmp_path, source, random_state=5))
    b = ModelTrainer().train(custom_config(tmp_path, source, random_state=5))
    assert a.metrics == b.metrics
    assert json.loads(a.metadata_path.read_text())["hyperparams"]["random_state"] == 5


def test_holdout_and_trade_mode_work_with_a_custom_estimator(tmp_path):
    source = write_estimator(tmp_path)
    meta = json.loads(ModelTrainer().train(custom_config(tmp_path, source, holdout_pct=20, trade_mode="long_short")).metadata_path.read_text())
    assert meta["holdout"]["rows"] > 0 and meta["trade_mode"] == "long_short"


@pytest.mark.parametrize(
    "text, fragment",
    [
        ("x = 1\n", "make_estimator"),
        ("def make_estimator(params):\n    return object()\n", "fit()"),
        ("class A:\n    def fit(self, X, y): return self\ndef make_estimator(params):\n    return A()\n", "predict()"),
        ("def make_estimator(params:\n", "never closed"),
        ("raise RuntimeError('boom at import')\n", "boom at import"),
    ],
)
def test_a_broken_estimator_file_is_a_clear_error_and_leaves_no_model(tmp_path, text, fragment):
    source = write_estimator(tmp_path, text, "broken.py")
    with pytest.raises(Exception, match=fragment):
        ModelTrainer().train(custom_config(tmp_path, source))
    models = tmp_path / "models"
    assert not models.exists() or not list(models.iterdir())


def test_custom_needs_an_existing_estimator_file(tmp_path):
    with pytest.raises(ValueError, match="estimator_file"):
        ModelTrainer().train(make_config(tmp_path, algorithm="custom", hyperparams={}))
    with pytest.raises(FileNotFoundError, match="gone.py"):
        ModelTrainer().train(make_config(tmp_path, algorithm="custom", hyperparams={}, estimator_file="gone.py", estimator_path=str(tmp_path / "gone.py")))


def test_the_trainer_cli_accepts_the_custom_fields(tmp_path):
    source = write_estimator(tmp_path)
    config = custom_config(tmp_path, source)
    payload = json.dumps({**config.__dict__})
    out = subprocess.run([sys.executable, "-m", "ml.trainer", "--config-json", payload], cwd=APEX_PYTHON, capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    assert json.loads(out.stdout.strip().splitlines()[-1])["model_id"].startswith("custom_")
    assert _config_from_json(json.loads(payload)).estimator_file == "centroids.py"
