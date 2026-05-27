class OpenKioku < Formula
  desc "Local-first code intelligence MCP for AI coding agents"
  homepage "https://github.com/shivyadavus/open-kioku"
  version "0.1.4"
  license "Elastic-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v0.1.4/ok-macos-arm64"
      sha256 "5b30f7cd552d3bfce7c946b01eb0937094e83c7d39357473007a42ac6df68cd1"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v0.1.4/ok-macos-x86_64"
      sha256 "f794acaa7d591deca43d67cd5ea7303c7b81336a37458f98cc2655a48bd34f86"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v0.1.4/ok-linux-arm64"
      sha256 "a102b034ba25e9255c370863ffe6c2b1e9aa9aa0d683562bb78155b278c8e64b"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v0.1.4/ok-linux-x86_64"
      sha256 "5c0eeb707bcbef715a1d92e456bc04d93f893b1b35f209f989a5c1d24883b0a8"
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
