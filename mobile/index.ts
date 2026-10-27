// App entry. expo-router's own entry, preceded by the one thing that must be
// registered before the app renders: the background task that clears read
// notifications from the tray (#1508). expo-task-manager may load this bundle
// headless — no UI, app killed — just to run that task, so it is defined at
// module scope here rather than inside a component.
import "./lib/push/dismissTask";
import "expo-router/entry";
