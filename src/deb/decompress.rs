use std::fmt;
use std::io::Read;

use crate::error::{Context, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Xz,
    Zstd,
}

impl Compression {
    /// Picks the compression from what follows ".tar" in a member name.
    pub fn from_suffix(member: &str, suffix: &str) -> Result<Compression> {
        Ok(match suffix {
            "" => Compression::None,
            ".gz" => Compression::Gzip,
            ".xz" => Compression::Xz,
            ".zst" => Compression::Zstd,
            _ => bail!("{member}: unsupported compression, expected none, .gz, .xz or .zst"),
        })
    }

    pub fn decoder<'a, R: Read + 'a>(self, r: R) -> Result<Box<dyn Read + 'a>> {
        Ok(match self {
            Compression::None => Box::new(r),
            Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(r)),
            Compression::Xz => Box::new(xz2::read::XzDecoder::new_multi_decoder(r)),
            Compression::Zstd => {
                Box::new(zstd::stream::read::Decoder::new(r).context("starting zstd decoder")?)
            }
        })
    }
}

impl fmt::Display for Compression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Compression::None => "none",
            Compression::Gzip => "gzip",
            Compression::Xz => "xz",
            Compression::Zstd => "zstd",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deb::testutil::compress;

    #[test]
    fn suffixes() {
        let cases = [
            ("", Some(Compression::None)),
            (".gz", Some(Compression::Gzip)),
            (".xz", Some(Compression::Xz)),
            (".zst", Some(Compression::Zstd)),
            (".bz2", None),
            (".lzma", None),
            ("x", None),
        ];
        for (suffix, want) in cases {
            let got = Compression::from_suffix("data.tar", suffix).ok();
            assert_eq!(got, want, "suffix '{suffix}'");
        }
    }

    #[test]
    fn round_trips_every_compression() {
        let payload = b"some payload that is long enough to compress a bit a bit a bit".repeat(10);
        for c in [Compression::None, Compression::Gzip, Compression::Xz, Compression::Zstd] {
            let packed = compress(&payload, c);
            let mut out = Vec::new();
            c.decoder(&packed[..]).unwrap().read_to_end(&mut out).unwrap();
            assert_eq!(out, payload, "{c}");
        }
    }
}
