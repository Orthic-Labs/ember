//! GoPro GPR (DNG with VC-5 coded raw data): the five public sample files of GoPro's reference implementation
//! (<https://github.com/gopro/gpr>, `data/samples`, Apache-2.0 OR MIT: not CC0, so not in `cargo run -p xtask -- corpus`).
//!
//! Copy them into `corpus/raw/` (or under `LIGHTCRAFT_CORPUS`) with the names below to run this test; each file that is
//! absent is skipped. A file is identified by its SHA-256 and must decode to the sensor mosaic whose FNV-1a hash (over the
//! decoded u16 samples, row-major, little-endian) was taken from a Python model of GoPro's decoder written separately from
//! this crate's; the model's picture of each file was looked at. The mosaic is 14-bit RGGB, so no sample exceeds 16383.

use lightcraft_raw::{RawData, RawFormat, decode, probe_info};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

/// File name in `corpus/raw`, the path in the GPR repository, SHA-256, mosaic width and height, FNV-1a of the samples.
const SAMPLES: [(&str, &str, &str, usize, usize, u64); 5] = [
    (
        "gopro-hero7-gopr9231.gpr",
        "data/samples/HERO7/GOPR9231.GPR",
        "0c2003137feb0ca7c9a8adf6cd832bc8fba7ff1b89cf5d77822b22558b9a20fe",
        4000,
        3000,
        0x7073_0a81_6059_27a3,
    ),
    (
        "gopro-hero5-gopr2657.gpr",
        "data/samples/Hero5/GOPR2657.GPR",
        "cf4bf90f2cff3ed36e2e51a632078537d1eddc1149279de4ecce1cb2c1879d5b",
        4000,
        3000,
        0xcfa0_dfdb_f82e_31ec,
    ),
    (
        "gopro-hero6-gopr0024.gpr",
        "data/samples/Hero6/GOPR0024.GPR",
        "82e8abdbe66db44e08f01078fe3174cd77d8a9414844f0526cd2d8177cec9778",
        4000,
        3000,
        0x9cd1_a61f_8a02_73f5,
    ),
    (
        "gopro-hero9-gopr0002.gpr",
        "data/samples/HERO9/GOPR0002.GPR",
        "9249208a3482d6d71d39575210fe19ab3dd737e5e446a4d0718d4bd9d3c4358b",
        5568,
        4176,
        0x96d6_29ea_10fb_5be3,
    ),
    (
        "gopro-fusion-gpbk7066.gpr",
        "data/samples/Fusion/GPBK7066.GPR",
        "c03eccdc334dc5b35cf6385799b219f4c9a0bc18ccbe70fe433237d98f352247",
        3104,
        3000,
        0x5608_4d0c_180a_a936,
    ),
];

#[test]
fn gopro_gpr_samples_decode_to_the_reference_mosaic() {
    for (name, source, sha, width, height, hash) in SAMPLES {
        let path = corpus().join("raw").join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} absent (GPR repository: {source})", path.display());
            continue;
        };
        let actual_sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(actual_sha, sha, "{name} is not {source}");
        let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(raw.format, RawFormat::Dng, "{name}");
        raw.validate().unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (width, height, 1), "{name}");
        assert_eq!(raw.info(), probe_info(&bytes).unwrap(), "{name}");
        let RawData::U16(samples) = &raw.data else { panic!("{name}: integer sensor data expected") };
        assert!(samples.iter().all(|&v| v <= 16383), "{name}: a sample above the 14-bit white level");
        let actual =
            samples.iter().flat_map(|v| v.to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, byte| (h ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3));
        assert_eq!(actual, hash, "{name}: the decoded mosaic differs from the reference model");
    }
}
