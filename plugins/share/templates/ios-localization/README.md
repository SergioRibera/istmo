# Localizing istmo.share's Info.plist keys

`istmo-build` merges `NSPhotoLibraryAddUsageDescription` (required by the
share sheet's "Save Image" action) into the app's `Info.plist` with an
English default. iOS localizes any `Info.plist` key through
`InfoPlist.strings`, whichever file the key came from:

1. Copy the `*.lproj/InfoPlist.strings` files you need into the app
   target (next to its other localized resources) and edit the text.
2. Add the languages to the project (`knownRegions` in xcodegen,
   *Project → Info → Localizations* in Xcode).

To change the default text itself, define the key in the app's
`Info.plist` outside the `istmo:plugins` markers — the app always wins
over plugin fragments.
