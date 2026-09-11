class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "4.0.0"
  license "Elastic-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/shivyadavus/open-kioku/releases/download/v4.0.0/ok-macos-arm64"
    sha256 "fb3651884fe7e81e4cb40aa9f929d46de3c16cd080226207bfd5ee8aa09273a8"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v4.0.0/ok-linux-arm64"
      sha256 "0bea285e2e8478be8cf904c6a5f8ea909b3f4c6669d15c249b4bcfe9febc08b0"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v4.0.0/ok-linux-x86_64"
      sha256 "ad18fc99a2d8ed84de194816d2b49e362faa940ffc861ff7b1a184a593adf75d"
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
