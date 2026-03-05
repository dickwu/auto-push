class AutoPush < Formula
  desc "CLI tool that automates git workflow with AI-generated commit messages"
  homepage "https://github.com/dickwu/auto-push"
  version "0.1.2"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/dickwu/auto-push/releases/download/v#{version}/auto-push-aarch64-apple-darwin.tar.gz"
      sha256 "a5e147b777502b034dad69c314af61eea8fa5cd8b1a00fdf20b65333557a78de"
    end

    on_intel do
      url "https://github.com/dickwu/auto-push/releases/download/v#{version}/auto-push-x86_64-apple-darwin.tar.gz"
      sha256 "8404ca6e8f480d7fbb6cb37688dddf2b0b11db129c69b7d7f4669b8c714ae82b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/dickwu/auto-push/releases/download/v#{version}/auto-push-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1ae2125178be223a27e6ad482e0dfb0acdcb37638e702feb3529ac9be381cb1a"
    end

    on_intel do
      url "https://github.com/dickwu/auto-push/releases/download/v#{version}/auto-push-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e09f16987fececa9e1f76364a7e1e11a61aff503b51ca8ba05d46043d356ecff"
    end
  end

  depends_on "gh"

  def install
    bin.install "auto-push"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/auto-push --version")
  end
end
