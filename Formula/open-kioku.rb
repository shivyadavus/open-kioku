class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "3.0.3"
  license "Elastic-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.3/ok-macos-arm64"
    sha256 "19145ce9c279e651a73de4c8a6d57c7dde1cbea5a8ce7ad99c7dc93292f1c1a7"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.3/ok-linux-arm64"
      sha256 "7172278a4829927b5ebdced76b28b612139e2e2f466f6cbe29a32228e8cc0e18"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.3/ok-linux-x86_64"
      sha256 "7d3d5a8b1ae148d1e0a4ac0f42929117bdefc32dd837dfde63f07d03b0e0d51b"
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
