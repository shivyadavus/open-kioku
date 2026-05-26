class OpenKioku < Formula
  desc "A blazing-fast, language-aware codebase index and semantic search engine for AI agents"
  homepage "https://shivyadavus.github.io/open-kioku/demo/index.html"
  version "0.1.0"
  license "Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v0.1.0/ok-macos-arm64"
      sha256 "REPLACE_ME_WITH_SHA256"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v0.1.0/ok-macos-x86_64"
      sha256 "REPLACE_ME_WITH_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shivyadavus/open-kioku/releases/download/v0.1.0/ok-linux-arm64"
      sha256 "REPLACE_ME_WITH_SHA256"
    else
      url "https://github.com/shivyadavus/open-kioku/releases/download/v0.1.0/ok-linux-x86_64"
      sha256 "REPLACE_ME_WITH_SHA256"
    end
  end

  def install
    if OS.mac? && Hardware::CPU.arm?
      bin.install "ok-macos-arm64" => "ok"
    elsif OS.mac? && Hardware::CPU.intel?
      bin.install "ok-macos-x86_64" => "ok"
    elsif OS.linux? && Hardware::CPU.arm?
      bin.install "ok-linux-arm64" => "ok"
    elsif OS.linux? && Hardware::CPU.intel?
      bin.install "ok-linux-x86_64" => "ok"
    end
  end

  test do
    system "#{bin}/ok", "--version"
  end
end
