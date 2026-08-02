mod link_tests {
    use crate::run::peer::configuration::absolute_peer_root;

    /// Caught on real hardware: the far side was sent `Users/xuanbomiao/x` and resolved it against
    /// the login home. A peer root has to arrive as the absolute path it was written as.
    #[test]
    fn a_posix_peer_root_gets_its_leading_slash_back() {
        assert_eq!(absolute_peer_root("Users/ben/Code"), "/Users/ben/Code");
        assert_eq!(absolute_peer_root("srv/data"), "/srv/data");
    }

    #[test]
    fn a_windows_peer_root_is_already_absolute() {
        assert_eq!(absolute_peer_root("C:/Users/ben"), "C:/Users/ben");
        assert_eq!(absolute_peer_root(r"D:\Code"), r"D:\Code");
    }
}
