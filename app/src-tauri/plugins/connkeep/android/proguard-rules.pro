# 本插件不参与混淆（Tauri 通过类名反射实例化，改名会直接找不到类）
-keep class com.wristry.app.connkeep.** { *; }
