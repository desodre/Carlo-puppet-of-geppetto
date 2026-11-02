package org.geppetto.carlo.fixture;

import android.app.Activity;
import android.os.Bundle;
import android.view.View;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;
import android.widget.Button;

/** App under test only: no instrumentation, service, IPC agent, or privileges. */
public final class FixtureActivity extends Activity {
    private int counter;
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        Button button = new Button(this);
        button.setText("Change Carlo fixture state");
        setContentView(button);
        View root = getWindow().getDecorView();
        root.setId(R.id.fixture_root);
        root.setAccessibilityDelegate(new View.AccessibilityDelegate() {
            @Override public void onInitializeAccessibilityNodeInfo(View host, AccessibilityNodeInfo info) {
                super.onInitializeAccessibilityNodeInfo(host, info);
                info.setText("CARLO_STATE_" + counter);
                info.setContentDescription("Carlo ação 🪵 <&>");
                info.setUniqueId("carlo-fixture-root");
                info.setEnabled(true);
                info.setClickable(true);
            }
        });
        button.setOnClickListener(view -> {
            counter++;
            root.sendAccessibilityEvent(AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED);
        });
    }
}
