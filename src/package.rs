use super::*;

#[derive(Debug, Snafu)]
#[snafu(context(suffix(false)), visibility(pub))]
pub enum Error {
  #[snafu(display("failed to deserialize manifest"))]
  DeserializeManifest {
    backtrace: Option<Backtrace>,
    source: ciborium::de::Error<io::Error>,
  },
  #[snafu(display("package file hash `{hash}` duplicated"))]
  FileHashDuplicated {
    hash: Hash,
    backtrace: Option<Backtrace>,
  },
  #[snafu(display("package file hash actually `{actual}` but expected `{expected}`"))]
  FileHashInvalid {
    actual: Hash,
    backtrace: Option<Backtrace>,
    expected: Hash,
  },
  #[snafu(display("package file hash `{hash}` out of order"))]
  FileHashOrder {
    hash: Hash,
    backtrace: Option<Backtrace>,
  },
  #[snafu(display("package file length `{len}` cannot be converted to usize"))]
  FileLengthRange {
    backtrace: Option<Backtrace>,
    len: u64,
    source: TryFromIntError,
  },
  #[snafu(display("I/O error reading file `{path}`"))]
  FileIo {
    backtrace: Option<Backtrace>,
    path: Utf8PathBuf,
    source: io::Error,
  },
  #[snafu(transparent)]
  Io {
    backtrace: Option<Backtrace>,
    source: io::Error,
  },
  #[snafu(display("I/O error copying from `{path}`"))]
  IoCopy {
    backtrace: Option<Backtrace>,
    path: Utf8PathBuf,
    source: io::Error,
  },
  #[snafu(display(
    "unexpected package magic bytes {} (\"{}\")",
    hex::encode(bytes),
    String::from_utf8_lossy(bytes)
  ))]
  MagicBytes {
    backtrace: Option<Backtrace>,
    bytes: Vec<u8>,
  },
  #[snafu(display("package contains {extra} extra files not accounted for in manifest"))]
  ManifestExtraFiles {
    extra: u64,
    backtrace: Option<Backtrace>,
  },
  #[snafu(display("manifest index {index} out of bounds of hash array"))]
  ManifestIndexOutOfBounds {
    backtrace: Option<Backtrace>,
    index: usize,
  },
  #[snafu(display("could not convert manifest index {index} to usize"))]
  ManifestIndexRange {
    backtrace: Option<Backtrace>,
    index: u64,
    source: TryFromIntError,
  },
  #[snafu(display("package missing {missing} files from manifest"))]
  ManifestMissingFiles {
    missing: u64,
    backtrace: Option<Backtrace>,
  },
  #[snafu(display("package has trailing {trailing} bytes"))]
  TrailingBytes {
    backtrace: Option<Backtrace>,
    trailing: u64,
  },
}

#[derive(Debug, PartialEq)]
pub struct Package {
  pub files: HashMap<Hash, Vec<u8>>,
  pub manifest: Manifest,
}

impl Package {
  pub const MAGIC_BYTES: &'static str = "MEDIA📦\0";

  pub fn load(path: &Utf8Path) -> Result<Self, Error> {
    let file = File::open(path)?;

    let len = file.metadata()?.len();

    let mut package = BufReader::new(file);

    let mut bytes = [0; Self::MAGIC_BYTES.len()];

    let mut read = 0;
    loop {
      let n = package.read(&mut bytes[read..])?;

      if n == 0 {
        break;
      }

      read += n;
    }

    ensure!(
      bytes == Self::MAGIC_BYTES.as_bytes(),
      MagicBytes {
        bytes: &bytes[..read],
      }
    );

    let index = package.read_u64()?;

    let index = usize::try_from(index).context(ManifestIndexRange { index })?;

    let hash_count = package.read_u64()?;

    let mut hashes = Vec::<(Hash, u64)>::new();

    for i in 0..hash_count {
      let hash = package.read_hash()?;
      let len = package.read_u64()?;

      usize::try_from(len).context(FileLengthRange { len })?;

      if let Some(last) = i.checked_sub(1) {
        let last = hashes[last as usize].0;
        ensure!(hash.as_bytes() >= last.as_bytes(), FileHashOrder { hash });

        ensure!(
          hash.as_bytes() != last.as_bytes(),
          FileHashDuplicated { hash }
        );
      }

      hashes.push((hash, len));
    }

    let manifest_hash = hashes
      .get(index)
      .context(ManifestIndexOutOfBounds { index })?
      .0;

    let mut files = HashMap::<Hash, Vec<u8>>::new();

    for (expected, len) in hashes {
      let mut buffer = vec![0; len as usize];

      package.read_exact(&mut buffer)?;

      let actual = blake3::hash(&buffer);

      ensure!(actual == expected, FileHashInvalid { expected, actual });

      files.insert(expected, buffer);
    }

    let position = package.stream_position()?;

    ensure!(
      position == len,
      TrailingBytes {
        trailing: len.saturating_sub(position),
      }
    );

    let manifest: Manifest = ciborium::from_reader(Cursor::new(files.get(&manifest_hash).unwrap()))
      .context(DeserializeManifest)?;

    manifest.verify(manifest_hash, &files)?;

    Ok(Self { manifest, files })
  }

  pub fn save(
    hashes: HashMap<Utf8PathBuf, (Hash, u64)>,
    manifest: &Manifest,
    output: &Utf8Path,
    root: &Utf8Path,
  ) -> Result<(), Error> {
    let mut package = BufWriter::new(File::create(output)?);

    package.write_all(super::Package::MAGIC_BYTES.as_bytes())?;

    let paths = hashes
      .iter()
      .map(|(path, (hash, _len))| (*hash, path.clone()))
      .collect::<HashMap<Hash, Utf8PathBuf>>();

    let mut hashes = hashes.values().copied().collect::<Vec<(Hash, u64)>>();

    let manifest = {
      let mut buffer = Vec::new();
      ciborium::into_writer(&manifest, &mut buffer).unwrap();
      buffer
    };

    let manifest_hash = blake3::hash(&manifest);

    hashes.push((manifest_hash, manifest.len().into_u64()));

    hashes.sort_by_key(|hash| *hash.0.as_bytes());

    let index = hashes
      .iter()
      .position(|(hash, _len)| *hash == manifest_hash)
      .unwrap()
      .into_u64();

    package.write_u64(index)?;

    package.write_u64(hashes.len().into_u64())?;

    for (hash, len) in &hashes {
      package.write_hash(*hash)?;
      package.write_u64(*len)?;
    }

    for (hash, _len) in hashes {
      if hash == manifest_hash {
        package.write_all(&manifest)?;
      } else {
        let path = root.join(paths.get(&hash).unwrap());

        let mut file = File::open(&path).context(FileIo { path: &path })?;

        io::copy(&mut file, &mut package).context(IoCopy { path: &path })?;
      }
    }

    Ok(())
  }

  pub fn file(&self, path: &str) -> Option<(Mime, Vec<u8>)> {
    match &self.manifest {
      Manifest::App { paths, .. } => Some((
        mime_guess::from_path(path).first_or_octet_stream(),
        self.files.get(paths.get(path)?).unwrap().clone(),
      )),
      Manifest::Comic { pages } => Some((
        mime::IMAGE_JPEG,
        self
          .files
          .get(pages.get(path.parse::<usize>().ok()?)?)
          .unwrap()
          .clone(),
      )),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn load_bad_magic_bytes() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    fs::write(&package, "this-is-not-a-package").unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::MagicBytes { bytes, .. }
      if bytes == *b"this-is-no"
    );
  }

  #[test]
  fn load_truncated_magic_bytes() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    fs::write(&package, "MEDIA").unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::MagicBytes { bytes, .. }
      if bytes == *b"MEDIA"
    );
  }

  #[test]
  fn manifest_index_out_of_bounds() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    let mut bytes = Vec::new();

    bytes.extend_from_slice(Package::MAGIC_BYTES.as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());

    fs::write(&package, bytes).unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::ManifestIndexOutOfBounds { index: 0, .. },
    );
  }

  #[test]
  fn file_hashes_out_of_order() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    let mut bytes = Vec::new();

    bytes.extend_from_slice(Package::MAGIC_BYTES.as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&2u64.to_le_bytes());
    bytes.extend_from_slice(&[1; 32]);
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&[0; 32]);
    bytes.extend_from_slice(&0u64.to_le_bytes());

    fs::write(&package, bytes).unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::FileHashOrder { hash, .. }
      if hash.as_bytes() == &[0; 32],
    );
  }

  #[test]
  fn file_hash_duplicated() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    let mut bytes = Vec::new();

    bytes.extend_from_slice(Package::MAGIC_BYTES.as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&2u64.to_le_bytes());
    bytes.extend_from_slice(&[0; 32]);
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&[0; 32]);
    bytes.extend_from_slice(&0u64.to_le_bytes());

    fs::write(&package, bytes).unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::FileHashDuplicated { hash, .. }
      if hash.as_bytes() == &[0; 32],
    );
  }

  #[test]
  fn file_hash_invalid() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    let mut bytes = Vec::new();

    bytes.extend_from_slice(Package::MAGIC_BYTES.as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&[0; 32]);
    bytes.extend_from_slice(&0u64.to_le_bytes());

    fs::write(&package, bytes).unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::FileHashInvalid { actual, expected, .. }
      if actual == blake3::hash(&[]) && expected.as_bytes() == &[0; 32],
    );
  }

  #[test]
  fn file_truncated() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    let mut bytes = Vec::new();

    bytes.extend_from_slice(Package::MAGIC_BYTES.as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&[0; 32]);
    bytes.extend_from_slice(&1u64.to_le_bytes());

    fs::write(&package, bytes).unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::Io { source, .. }
      if source.kind() == io::ErrorKind::UnexpectedEof,
    );
  }

  #[test]
  fn trailing_bytes() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    let mut bytes = Vec::new();

    bytes.extend_from_slice(Package::MAGIC_BYTES.as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(blake3::hash(&[]).as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&[0]);

    fs::write(&package, bytes).unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::TrailingBytes { trailing: 1, .. },
    );
  }

  #[test]
  fn manifest_deserialize_error() {
    let tempdir = tempdir();

    let package = tempdir.path_utf8().join("package.package");

    let mut bytes = Vec::new();

    bytes.extend_from_slice(Package::MAGIC_BYTES.as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(blake3::hash(&[]).as_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());

    fs::write(&package, bytes).unwrap();

    assert_matches!(
      Package::load(&package).unwrap_err(),
      Error::DeserializeManifest { .. },
    );
  }

  #[test]
  fn save_and_load() {
    let tempdir = tempdir();

    let output = tempdir.path_utf8().join("package.package");

    let root = tempdir.path_utf8().join("root");

    fs::create_dir(&root).unwrap();
    fs::write(root.join("index.html"), "html").unwrap();
    fs::write(root.join("index.js"), "js").unwrap();

    let html = blake3::hash(b"html");
    let js = blake3::hash(b"js");

    let manifest = Manifest::App {
      handles: Type::Comic,
      paths: vec![("index.html".into(), html), ("index.js".into(), js)]
        .into_iter()
        .collect(),
    };

    let manifest_bytes = {
      let mut buffer = Vec::new();
      ciborium::into_writer(&manifest, &mut buffer).unwrap();
      buffer
    };

    let hashes = vec![
      ("index.html".into(), (html, 4)),
      ("index.js".into(), (js, 2)),
    ]
    .into_iter()
    .collect();

    Package::save(hashes, &manifest, &output, &root).unwrap();

    assert_eq!(
      Package::load(&output).unwrap(),
      Package {
        files: vec![
          (html, b"html".into()),
          (js, b"js".into()),
          (blake3::hash(&manifest_bytes), manifest_bytes)
        ]
        .into_iter()
        .collect(),
        manifest,
      },
    );
  }
}
