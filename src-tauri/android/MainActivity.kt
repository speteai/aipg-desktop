package io.github.speteai.aipgdesktop

import android.graphics.Color
import android.os.Bundle
import android.view.View
import android.webkit.WebView
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

// Replaces the generated MainActivity (copied over it in CI).
// Android draws the app under the status bar and the navigation bar. The page
// is kept out of both, and out of the way of the keyboard, by padding the
// content view with those insets; the padding shows the app's dark background.
class MainActivity : TauriActivity() {
  // The web view is white until the page has drawn; give it the app's dark
  // background so the start does not flash.
  override fun onWebViewCreate(webView: WebView) {
    webView.setBackgroundColor(Color.parseColor("#141311"))
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge(
      SystemBarStyle.dark(Color.TRANSPARENT),
      SystemBarStyle.dark(Color.TRANSPARENT)
    )
    super.onCreate(savedInstanceState)
    window.decorView.setBackgroundColor(Color.parseColor("#141311"))
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
      val i = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or
          WindowInsetsCompat.Type.displayCutout() or
          WindowInsetsCompat.Type.ime()
      )
      v.setPadding(i.left, i.top, i.right, i.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }
}
