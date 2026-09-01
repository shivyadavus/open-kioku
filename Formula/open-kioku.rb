class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "3.1.0"
  license "Elastic-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.1.0/ok-macos-arm64"
    sha256 "1c0a746cd2feb7af3e6b0eef59a9196c08170ed175ff5280a1683d8055eb0f2a"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.1.0/ok-linux-arm64"
      sha256 "a4244be41c0c7915b05da0f0818a002ad6e8571e8013cb7bdaf926435ee3d624"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.1.0/ok-linux-x86_64"
      sha256 "d99b5476f8a7b0072bd4efc0bf5dffc26967eaa3692688cf42e8ce92f10fcbec"
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
