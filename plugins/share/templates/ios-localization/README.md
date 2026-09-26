# Localizing istmo.share's Info.plist keys

istmo.share declares `NSPhotoLibraryAddUsageDescription` (required by
the share sheet's "Save Image" action) in its `[plugin.info_plist]`, so
`istmo-build` writes it into the app's `Info.plist` with an English
default. iOS localizes any `Info.plist` key through
`InfoPlist.strings`, whichever file the key came from:

1. Copy the `*.lproj/InfoPlist.strings` files you need into the app
   target (next to its other localized resources) and edit the text.
2. Add the languages to the project (`knownRegions` in xcodegen,
   *Project → Info → Localizations* in Xcode).

To change the default text itself, override it in the app's
`istmo.toml`:

```toml
[app.info_plist]
NSPhotoLibraryAddUsageDescription = "Save photos your friends send you."
```
