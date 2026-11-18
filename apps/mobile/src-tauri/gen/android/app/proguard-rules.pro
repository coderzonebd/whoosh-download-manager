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
# The web UI calls these through `WdmAndroid` (MainActivity.Bridge); keep them in release builds.
-keepclassmembers class com.coderzonebd.whoosh.MainActivity$Bridge {
    @android.webkit.JavascriptInterface <methods>;
}

# youtubedl-android (MediaTools.kt): its updater reads GitHub's JSON with Jackson, through
# reflection that R8 cannot see.
-keep class com.yausername.** { *; }
-keep class com.fasterxml.jackson.** { *; }
-dontwarn com.fasterxml.jackson.**
# Its unzip (Apache Commons Compress, Commons IO) finds classes by reflection: R8 left an abstract
# class where a concrete one was looked up ("class … is not a concrete class").
-keep class org.apache.commons.** { *; }
-dontwarn org.apache.commons.**
