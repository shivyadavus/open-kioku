class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "3.0.4"
  license "Elastic-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.4/ok-macos-arm64"
    sha256 "3f8f6c217aa3cf26fe14945d586c0beff76b95fffe05840ec7526ce05679321e"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.4/ok-linux-arm64"
      sha256 "868b4d2c431844ddd000a16d4f0998dcb2f548a058aa694602c7bc3494a8ed39"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.4/ok-linux-x86_64"
      sha256 "7a7bef03fdeea0282c9df8d0fe9d3f409095b581e4b7f1a33fd57c8d480cdc16"
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
