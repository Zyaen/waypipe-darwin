class WaypipeDarwin < Formula
  desc "Proxy for Wayland clients (optimized for macOS/Darwin)"
  homepage "https://github.com/J-x-Z/waypipe-darwin"
  url "https://github.com/J-x-Z/waypipe-darwin.git", branch: "main"
  version "0.9.2-darwin"
  head "https://github.com/J-x-Z/waypipe-darwin.git", branch: "main"

  depends_on "rust" => :build
  depends_on "lz4"
  depends_on "zstd"

  conflicts_with "waypipe", because: "both install `waypipe` binaries"

  def install
    system "cargo", "install", *std_cargo_args, "--no-default-features", "--features", "lz4,zstd"
  end

  test do
    assert_match "waypipe", shell_output("#{bin}/waypipe --version")
  end
end
