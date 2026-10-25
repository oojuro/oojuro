class Ojo < Formula
  desc "Inspect and search portable .juro databases"
  homepage "https://github.com/oojuro/oojuro"
  url "https://github.com/oojuro/oojuro/releases/download/ojo-v0.2.0/ojo-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
  version "0.2.0"
  sha256 "865c5411c95c46a892df14d32e07d8a7a8966ac8fea09537f68c471e2bdd3056"
  license "Apache-2.0"

  depends_on :linux
  depends_on arch: :x86_64

  def install
    bin.install "bin/ojo"
    pkgshare.install "LICENSE", "NOTICE", "Cargo.lock", "manifest.json", "licenses"
  end

  test do
    system bin/"ojo", "init", testpath/"sensors.juro"
    (testpath/"sample.txt").write("sensor calibration confirmed\n")
    system bin/"ojo", "append", testpath/"sensors.juro", testpath/"sample.txt"
    system bin/"ojo", "keygen", testpath/"search.key"
    system bin/"ojo", "index", testpath/"sensors.juro", testpath/"search-index",
           "--index-key", testpath/"search.key"
    result = shell_output("#{bin}/ojo search #{testpath}/sensors.juro #{testpath}/search-index calibration --index-key #{testpath}/search.key")
    assert_match "sensor calibration confirmed", result
  end
end
