class Ojo < Formula
  desc "Inspect, verify and manipulate portable .juro databases"
  homepage "https://github.com/oojuro/oojuro"
  url "https://github.com/oojuro/oojuro/releases/download/ojo-v0.1.0/ojo-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
  version "0.1.0"
  sha256 "08dc699737322a586ce1451f4eb6f7e7210a6370b9b2413ba5a87a806c325fdf"
  license "Apache-2.0"

  depends_on :linux
  depends_on arch: :x86_64

  def install
    bin.install "bin/ojo"
    pkgshare.install "LICENSE", "NOTICE", "Cargo.lock", "manifest.json", "licenses"
  end

  test do
    system bin/"ojo", "create", testpath/"empty.juro"
    system bin/"ojo", "verify", testpath/"empty.juro"
  end
end
