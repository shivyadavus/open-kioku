class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "3.0.1"
  license "Elastic-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.1/ok-macos-arm64"
    sha256 "ea167a7521443cd889af6a8fd6017859eb395ec6ed34a10bc361f300dec89dcc"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.1/ok-linux-arm64"
      sha256 "5e1fb0449eabc63be0f943b1f9a8fcdcd7d7cadb4beae2831686808a05bf459c"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.1/ok-linux-x86_64"
      sha256 "fdafec54fdf29e5685fe25ea34244c5de04b4960ec51de18b5589a60f86b782f"
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
