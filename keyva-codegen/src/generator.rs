//! Generator trait and output types.

use crate::spec::ProtocolSpec;
use std::path::Path;

/// A named output file produced by a generator.
pub struct GeneratedFile {
    /// Relative path within the output directory.
    pub path: String,
    /// File contents.
    pub content: String,
}

/// Trait implemented by each language generator.
pub trait Generator {
    /// Human-readable name of the target language (e.g. "Python", "TypeScript").
    fn language(&self) -> &'static str;

    /// Generate all output files from the protocol spec.
    fn generate(&self, spec: &ProtocolSpec) -> Vec<GeneratedFile>;
}

/// Write all generated files to the output directory.
pub fn write_output(files: &[GeneratedFile], output_dir: &Path) -> std::io::Result<()> {
    for file in files {
        let path = output_dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.content)?;
    }
    Ok(())
}
