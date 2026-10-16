//! An immutable AI repair asset, referenced by the edit description and its history.
use lightcraft_geom::{Point, Rect};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiPatch {
    /// Content-addressed, lossless generated pixels; never an original image file.
    pub id: String,
    pub model: String,
    /// Hash of the original's content identity. A repair cannot silently migrate to another photo.
    pub source: String,
    /// Uncropped, EXIF-oriented source coordinates, before user rotation/lens/perspective.
    pub bounds: Rect,
    pub width: usize,
    pub height: usize,
    /// Generation recipe in the same source coordinates, for explicit Regenerate.
    pub stroke: Vec<Point>,
    pub radius: f64,
}
