class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "3.0.3"
  license "Elastic-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.3/ok-macos-arm64"
    sha256 "979dfb43077d054b2bbfb83a0bc84ce3db93842a304216a56eb052dd02a5b44e"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.3/ok-linux-arm64"
      sha256 "9859635c31dca4c5ab56eda28c5fb012fb11acd20ffc988a856489ff18bf1bdc"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.3/ok-linux-x86_64"
      sha256 "7561eeb1c2ded349ba0a59e3218265590a0b32be854fc58eebd91a415be13b07"
    end
  end

  def install
    binary = Dir["ok-*"].first
    chmod 0755, binary
    bin.install binary => "ok"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ok --version")
    assert_match "doctor", shell_output("#{bin}/ok --help")
  end
end
