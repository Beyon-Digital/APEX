"""Inference for a trained APEX model.

Loads a ``.joblib`` artifact persisted by :mod:`ml.trainer` and scores feature rows supplied as
JSON. Prints a single JSON line.

Single row (``--features-json``):

    {"signal": <int>, "probability": <float>, "model_id": <str>, "features_used": [...]}

Many rows in one call (``--features-matrix-json``, a JSON array of rows in metadata feature order):

    {"signals": [<int>...], "probabilities": [<float|null>...], "model_id": <str>, "features_used": [...]}

Used by the Rust automation engine — the model's joblib file lives next to a
companion ``<model_id>.json`` metadata file describing feature ordering.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import joblib

from ml.estimators import estimator_module_name, import_estimator_file


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Predict with an APEX model")
    parser.add_argument("--model-path", required=True)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument(
        "--features-json",
        help="JSON object mapping feature name -> value, or a JSON array in metadata feature order",
    )
    group.add_argument(
        "--features-matrix-json",
        help="JSON array of rows, each a list of values in metadata feature order (batch mode)",
    )
    return parser.parse_args()


def _load_model(model_path: Path, meta: dict):
    """Load the joblib artifact, first making a custom estimator's code importable.

    A model trained with a custom estimator pickles instances of classes defined in the user's
    file. The copy saved next to the model is imported by file location under the module name
    the pickle refers to, so it resolves even if the original file was edited or deleted.
    """
    copy = meta.get("estimator_copy")
    if copy:
        copy_path = model_path.parent / str(copy)
        if Path(str(copy)).name != str(copy) or not copy_path.is_file():
            raise FileNotFoundError(f"this model's estimator file is missing: {copy}")
        sys.path.insert(0, str(model_path.parent))
        module_name = meta.get("estimator_module") or estimator_module_name(str(meta.get("estimator_sha256") or ""))
        import_estimator_file(copy_path, module_name)
    return joblib.load(model_path)


def _probabilities(model, rows) -> list[float | None]:
    """Probability of the predicted class for each row, or None when unavailable."""
    if not hasattr(model, "predict_proba"):
        return [None] * len(rows)
    try:
        return [float(max(p)) for p in model.predict_proba(rows)]
    except Exception:
        return [None] * len(rows)


def _predict_batch(model, model_path: Path, feature_order: list[str], raw_matrix: str) -> int:
    matrix = json.loads(raw_matrix)
    if not isinstance(matrix, list) or not all(isinstance(r, list) for r in matrix):
        print(json.dumps({"error": "features matrix must be an array of rows"}))
        return 1
    width = len(feature_order)
    if width and any(len(r) != width for r in matrix):
        print(json.dumps({"error": f"every row needs {width} values (feature order: {', '.join(feature_order)})"}))
        return 1
    rows = [[float(v) for v in r] for r in matrix]
    if not rows:
        signals, probabilities = [], []
    else:
        signals = [int(p) for p in model.predict(rows)]
        probabilities = _probabilities(model, rows)
    print(
        json.dumps(
            {
                "signals": signals,
                "probabilities": probabilities,
                "model_id": model_path.stem,
                "features_used": feature_order,
            }
        )
    )
    return 0


def main() -> int:
    args = _parse_args()
    model_path = Path(args.model_path)
    metadata_path = model_path.with_suffix(".json")

    feature_order: list[str] = []
    meta: dict = {}
    if metadata_path.exists():
        meta = json.loads(metadata_path.read_text())
        feature_order = list(meta.get("feature_names") or [])

    if args.features_matrix_json is not None:
        return _predict_batch(_load_model(model_path, meta), model_path, feature_order, args.features_matrix_json)

    raw = json.loads(args.features_json)
    if isinstance(raw, dict):
        if not feature_order:
            feature_order = sorted(raw)
        try:
            row = [[float(raw[name]) for name in feature_order]]
        except KeyError as exc:
            missing = ", ".join(sorted(set(feature_order) - set(raw)))
            print(json.dumps({"error": f"missing features: {missing} (offending: {exc})"}))
            return 1
    else:
        row = [[float(v) for v in raw]]

    model = _load_model(model_path, meta)
    pred = model.predict(row)[0]

    probability = _probabilities(model, row)[0]

    print(
        json.dumps(
            {
                "signal": int(pred),
                "probability": probability,
                "model_id": model_path.stem,
                "features_used": feature_order,
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
