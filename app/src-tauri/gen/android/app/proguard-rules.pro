# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile

# --- FocusKitty ------------------------------------------------------------
#
# Release builds minify, and R8 renames anything it cannot see being used.
# Two things here are reached by NAME at runtime, which R8 cannot see:
#
#   1. The Rust core asks the app's class loader for
#      "com.siva.focuskitty.CatOverlayService" and calls its static
#      isRunning() -- that is how the app knows whether the cat is on screen.
#   2. Every quick-tool button calls a Bridge method from JavaScript by name.
#
# Renamed, the cat's on/off state and every button in the panel stop working
# in release and only in release -- which is the worst kind of bug to find.
-keep class com.siva.focuskitty.CatOverlayService { *; }
-keep class com.siva.focuskitty.CatOverlayService$* { *; }

# Any @JavascriptInterface method, wherever it lives.
-keepclassmembers class * {
    @android.webkit.JavascriptInterface <methods>;
}
