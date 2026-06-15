class Katok < Formula
  desc "Local-first KakaoTalk memory archive and search CLI for macOS"
  homepage "https://github.com/NomaDamas/katok"
  url "git@github.com:NomaDamas/katok.git",
    tag:      "v0.1.0",
    revision: "c42310877fdee60c37394b8fecf3d98d9114a0c1"
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
