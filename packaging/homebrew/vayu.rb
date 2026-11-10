# TEMPLATE cask: see README.md. Version, sha256 and every URL are placeholders.
cask "vayu" do
  version "0.0.1"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/coderzonebd/vayu/releases/download/v#{version}/Vayu_#{version}_universal.dmg",
      verified: "github.com/coderzonebd/vayu/"
  name "Vayu"
  desc "Fast, open-source download manager"
  homepage "https://github.com/coderzonebd/vayu"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: ">= :monterey"

  app "Vayu.app"

  zap trash: [
    "~/Library/Application Support/com.coderzonebd.vayu",
    "~/Library/Caches/com.coderzonebd.vayu",
    "~/Library/HTTPStorages/com.coderzonebd.vayu",
    "~/Library/Preferences/com.coderzonebd.vayu.plist",
    "~/Library/Saved Application State/com.coderzonebd.vayu.savedState",
    "~/Library/WebKit/com.coderzonebd.vayu",
  ]
end
