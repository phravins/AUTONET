# AutoNet, as a Homebrew formula.
#
# THE SHA256 VALUES BELOW ARE PLACEHOLDERS. They are sixty-four zeros, which is
# a well-formed hash that no file will ever have, so `brew install` fails with a
# mismatch rather than installing something unverified. They cannot be filled in
# until a release exists, because there is nothing yet to hash: this repository
# has no tags. After the first `git push --tags`, take them from the release's
# own SHA256SUMS:
#
#     curl -fsSL https://github.com/phravins/AUTONET/releases/latest/download/SHA256SUMS
#
# and paste each line's hash next to the matching filename below. The filenames
# here are exactly the ones .github/workflows/release.yml produces; if you find
# yourself editing a filename to make it match, the workflow changed and this
# file is now wrong in more places than one.
#
# There is no tap yet, so this installs from the path:
#
#     brew install --formula ./packaging/homebrew/autonet.rb
#
# Creating phravins/homebrew-tap would make it `brew install phravins/tap/autonet`;
# that needs a second repository, which is not something this file can do.
class Autonet < Formula
  desc "Find the LAN address a service is actually reachable on"
  homepage "https://github.com/phravins/AUTONET"
  version "0.1.0"
  # Matches the `license = "MIT OR Apache-2.0"` in Cargo.toml, and the
  # LICENSE-MIT and LICENSE-APACHE files each archive carries.
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

  # Each archive contains one directory, `autonet-<version>-<target>/`, holding
  # the binary and the two licences. Homebrew descends into a lone top-level
  # directory on its own, so the binary is simply here.
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
