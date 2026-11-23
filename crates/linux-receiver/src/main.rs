fn main() {
    meshwav_receiver::cli::run_cli("meshwav-receiver", std::env::args().skip(1).collect());
}
