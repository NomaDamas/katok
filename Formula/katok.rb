class Katok < Formula
  desc "Local KakaoTalk keyword, BM25, and vector search CLI for Apple Silicon macOS"
  homepage "https://github.com/NomaDamas/katok"
  url "https://github.com/NomaDamas/katok.git",
    tag:      "v0.3.0",
    revision: "5c77b9afb096ff4ae38d60368b18441e88e8ed32"
  license "MIT"

  depends_on "rust" => :build
  depends_on arch: :arm64

  def install
    system "cargo", "install", *std_cargo_args
  end

  def caveats
    <<~EOS
      For native KakaoTalk sync, grant your terminal Full Disk Access:
        System Settings > Privacy & Security > Full Disk Access

      Then run:
        katok doctor --json
    EOS
  end

  test do
    assert_match "katok", shell_output("#{bin}/katok --help")
  end
end
