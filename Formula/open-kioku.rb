class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "3.0.2"
  license "Elastic-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.2/ok-macos-arm64"
    sha256 "699fabe7a2f7f370aceac533b195218bb7a3c42a3e6004f3a32ae719b528a307"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.2/ok-linux-arm64"
      sha256 "a3ca199ed5d04ac7d434bdc584b9636955fbeaf26d22990d4c1bbaf60495c75c"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.2/ok-linux-x86_64"
      sha256 "b08422c48da06e20ae97708d831719f3a12a54d12f8c8d985058391afe604736"
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
