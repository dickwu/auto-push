class AutoPush < Formula
  desc "CLI tool that automates git workflow with AI-generated commit messages"
  homepage "https://github.com/dickwu/auto-push"
  version "0.1.5"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/dickwu/auto-push/releases/download/v#{version}/auto-push-aarch64-apple-darwin.tar.gz"
      sha256 "75985f39baebe07c953571d34e2e2a319d0cb99f24422f4a04aaf35302e70bf8"
    end

    on_intel do
      url "https://github.com/dickwu/auto-push/releases/download/v#{version}/auto-push-x86_64-apple-darwin.tar.gz"
      sha256 "81adb298a18757c20ce11f68566222aa2ee434f3876ec12a016e08edb2e312f7"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/dickwu/auto-push/releases/download/v#{version}/auto-push-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "46cccaec2e57bd5962b413e787d7e9acee039520ac072d122dd70d0d3120d967"
    end

    on_intel do
      url "https://github.com/dickwu/auto-push/releases/download/v#{version}/auto-push-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9e4df5a1b6d5cd60afee5d87cae4a94fe507496a60ca9184bb88bad6fcaa2035"
    end
  end

  def install
    bin.install "auto-push"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/auto-push --version")
  end
end
