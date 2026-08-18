class OpenKioku < Formula
  desc "Local-first code intelligence for AI agents. Plan before edit. Verify after edit."
  homepage "https://github.com/shivyadavus/open-kioku"
  version "3.0.0"
  license "Elastic-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-macos-arm64"
    sha256 "85922cbad9f623ff8f6f85fba4c0670e6ab9fb0b6d3b46e612d96232a2e8c82d"
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-linux-arm64"
      sha256 "35868a85311749b8d4e9ccf6b1e721383094a037c1a98fd93ece18b6aa63e1b7"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-linux-x86_64"
      sha256 "59cfb3dc68f8a9c803587145cc2b39a0baa10c9f74cb0b38c0e6136155c8124b"
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
