//! Per-container vanilla extract and rebuild: dump a single game container's pak + utoc/ucas content to a legacy tree, then rebuild a modified tree back into a swap-ready container set.

pub(crate) mod extract;
pub(crate) mod rebuild;
