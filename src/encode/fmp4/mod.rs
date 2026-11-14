//! Fragmented MP4, written by PolyRec itself, so a recording survives a
//! crash. Media Foundation's MP4 sink writes the file's index only when it
//! finishes, so a recording that never finishes (PolyRec killed, a crash,
//! a power cut, a driver reset) can't be read at all; its fragmented MP4
//! sink, which can, takes H.264 only, and HEVC is PolyRec's default. So a
//! media sink of PolyRec's own (`sink`) takes the sink writer's place
//! behind the encoders, which stay Media Foundation's, and writes the file
//! a fragment (one GOP, about a second) at a time: a recording cut off
//! loses its last fragment, no more.
//!
//! At Stop the file is finished in place (`soft_remux`): a complete index
//! goes at its end and a placeholder near its start becomes the header of
//! one `mdat` holding everything before it -- a plain MP4, in about a
//! second however long the recording, without copying it. A recording cut
//! off is finished the same way at the next start. A file without the
//! placeholder -- written by a PolyRec before 0.5.162, or not a recording
//! at all -- isn't touched: the remux that finished those then went with
//! 0.5.172, ten releases on.

pub mod boxes;
pub mod muxer;
pub mod nal;
mod read;
pub mod sink;
pub mod soft_remux;

use crate::error::AppError;
use soft_remux::InPlace;
use std::path::{Path, PathBuf};

/// Finishes the recording at `temp` into a plain MP4 and moves it to the
/// name `reserve` gives -- asked only once the file is finished, so a
/// finish cut off never leaves an empty file holding a name. Returns where
/// it's saved. A recording that can't be finished in place is moved there
/// as it is: a fragmented MP4 plays all the same. A file that isn't one of
/// today's recordings (`InPlace::NotOurs`) is an error, and left where it
/// is.
pub fn finish(
    temp: &Path,
    reserve: impl FnOnce() -> Result<PathBuf, AppError>,
) -> Result<PathBuf, AppError> {
    match soft_remux::finish_in_place(temp) {
        InPlace::Finished | InPlace::KeptFragmented => {
            into_reserved(reserve()?, |dest| move_over(temp, dest))
        }
        InPlace::NotOurs => Err(AppError::Encode(format!(
            "{} isn't a recording this PolyRec can finish (written before 0.5.162, or not a recording)",
            temp.display()
        ))),
    }
}

/// `dest`, once `fill` has put the recording there; the empty file that
/// reserved it is removed if it couldn't.
fn into_reserved(
    dest: PathBuf,
    fill: impl FnOnce(&Path) -> Result<(), AppError>,
) -> Result<PathBuf, AppError> {
    fill(&dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })?;
    Ok(dest)
}

/// Moves the recording at `from` to `dest`, over the empty file that
/// reserved that name.
fn move_over(from: &Path, dest: &Path) -> Result<(), AppError> {
    use crate::error::ResultExt;
    std::fs::rename(from, dest).encode_ctx(format_args!(
        "moving {} to {}",
        from.display(),
        dest.display()
    ))
}

#[cfg(test)]
pub(crate) mod tests;
