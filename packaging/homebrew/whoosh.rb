# TEMPLATE cask: see README.md. Version, sha256 and every URL are placeholders.
cask "whoosh" do
  version "0.0.1"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/coderzonebd/whoosh/releases/download/v#{version}/Whoosh_#{version}_universal.dmg",
      verified: "github.com/coderzonebd/whoosh/"
  name "Whoosh"
  desc "Fast, open-source download manager"
  homepage "https://github.com/coderzonebd/whoosh"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: ">= :monterey"

  app "Whoosh.app"

  zap trash: [
    "~/Library/Application Support/com.coderzonebd.whoosh",
    "~/Library/Caches/com.coderzonebd.whoosh",
    "~/Library/HTTPStorages/com.coderzonebd.whoosh",
    "~/Library/Preferences/com.coderzonebd.whoosh.plist",
    "~/Library/Saved Application State/com.coderzonebd.whoosh.savedState",
    "~/Library/WebKit/com.coderzonebd.whoosh",
  ]
end
