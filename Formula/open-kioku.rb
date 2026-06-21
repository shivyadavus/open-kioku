class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "2.1.1"
  license "Elastic-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v2.1.1/ok-macos-arm64"
      sha256 "2db7ab0b77293988349b27da72cbdde2f37ddf5d897ef13ec3020cadc3fb90e0"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v2.1.1/ok-macos-x86_64"
      sha256 "5bbcd44ccfee2fd304fc1887939e6374830c365802684cf46af79e9453ccc4a7"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v2.1.1/ok-linux-arm64"
      sha256 "1a88a5031ef0c66a704c2c7f5469c0ed5f1a808da2baea305083e3237b9619b3"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v2.1.1/ok-linux-x86_64"
      sha256 "ecc4c75971333587cb8fb662783f3623fd659ce03f29c153692ec231b2c62cd4"
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
