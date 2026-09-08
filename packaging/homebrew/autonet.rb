# AutoNet, as a Homebrew formula. Optional: the one-line installer in
# scripts/install.sh needs no package manager.
#
# THE SHA256 VALUES BELOW ARE PLACEHOLDERS -- sixty-four zeros, well-formed but
# matching no file, so `brew install` fails rather than installing something
# unverified. There is nothing to hash yet; this repository has no tags. After
# the first tag push, fill them in from the release's own checksums:
#
#     curl -fsSL https://github.com/phravins/AUTONET/releases/latest/download/SHA256SUMS
#
# The filenames here are exactly the ones .github/workflows/release.yml
# produces. No tap exists yet, so this installs from the path:
#
#     brew install --formula ./packaging/homebrew/autonet.rb
class Autonet < Formula
  desc "Find the LAN address a service is actually reachable on"
  homepage "https://github.com/phravins/AUTONET"
  version "0.1.0"
  # Matches `license = "MIT OR Apache-2.0"` in Cargo.toml.
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/phravins/AUTONET/releases/download/v0.1.0/autonet-0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/phravins/AUTONET/releases/download/v0.1.0/autonet-0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/phravins/AUTONET/releases/download/v0.1.0/autonet-0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/phravins/AUTONET/releases/download/v0.1.0/autonet-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  # Each archive holds one directory, `autonet-<version>-<target>/`, and
  # Homebrew descends into a lone top-level directory on its own.
  def install
    bin.install "autonet"
  end

  def caveats
    <<~EOS
      AutoNet is not code-signed or notarized.

      Homebrew's own downloader does not attach the com.apple.quarantine
      attribute, so an install through brew normally runs without any extra
      step. If you instead download a release tarball with a web browser,
      macOS will refuse to run the binary until you clear it:

          xattr -d com.apple.quarantine $(which autonet)

      Nothing AutoNet does needs elevated privileges. It reads network state
      and, with `autonet advertise`, sends multicast DNS on the local link.
      It opens no ports for inbound connections and changes no system
      configuration.
    EOS
  end

  test do
    assert_match "autonet #{version}", shell_output("#{bin}/autonet --version")
  end
end
