mod temporary_package_tests {
    use crate::run::peer::package::TemporaryPeerPackage;
    use std::io::{Read, Write};

    #[test]
    fn peer_package_is_exclusive_reopen_free_and_removed_on_drop() {
        let package = TemporaryPeerPackage::create().unwrap();
        let path = package.root.display_path().join(package.file_name());
        let mut output = package.output().unwrap();
        output.write_all(b"package").unwrap();
        output.sync_all().unwrap();

        let mut reader = package.reader().unwrap();
        let mut contents = Vec::new();
        reader.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"package");
        assert_eq!(package.len().unwrap(), b"package".len() as u64);
        assert!(path.is_file());

        drop(reader);
        drop(output);
        drop(package);
        assert!(!path.exists());
    }

    #[test]
    fn concurrent_peer_packages_never_share_a_name() {
        let first = TemporaryPeerPackage::create().unwrap();
        let second = TemporaryPeerPackage::create().unwrap();

        assert_ne!(first.file_name(), second.file_name());
    }
}
