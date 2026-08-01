use serde::Serialize;

pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StealthProfile {
    pub enabled: bool,
    pub user_agent: Option<String>,
    pub window_width: u32,
    pub window_height: u32,
}

impl StealthProfile {
    pub fn new(enabled: bool, user_agent: Option<String>) -> Self {
        Self {
            enabled,
            user_agent,
            window_width: 1365,
            window_height: 768,
        }
    }

    pub fn chrome_args(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }

        vec![
            "--disable-blink-features=AutomationControlled".to_owned(),
            format!("--window-size={},{}", self.window_width, self.window_height),
            format!(
                "--user-agent={}",
                self.user_agent
                    .as_deref()
                    .unwrap_or(DEFAULT_USER_AGENT)
                    .replace("HeadlessChrome", "Chrome")
            ),
        ]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StealthScript {
    pub name: &'static str,
    pub source: &'static str,
}

pub fn baseline_scripts() -> &'static [StealthScript] {
    &[StealthScript {
        name: "baseline-navigator-and-webgl",
        source: BASELINE_SCRIPT,
    }]
}

pub const BASELINE_SCRIPT: &str = r#"
(() => {
  const defineGetter = (object, property, getter) => {
    try {
      Object.defineProperty(object, property, { get: getter, configurable: true });
    } catch (_) {}
  };

  defineGetter(Navigator.prototype, 'webdriver', () => undefined);
  defineGetter(navigator, 'webdriver', () => undefined);
  defineGetter(Navigator.prototype, 'languages', () => ['en-US', 'en']);
  defineGetter(navigator, 'languages', () => ['en-US', 'en']);
  defineGetter(Navigator.prototype, 'plugins', () => [
    { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
    { name: 'Chrome PDF Viewer', filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai', description: '' },
    { name: 'Native Client', filename: 'internal-nacl-plugin', description: '' }
  ]);
  defineGetter(navigator, 'plugins', () => [
    { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
    { name: 'Chrome PDF Viewer', filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai', description: '' },
    { name: 'Native Client', filename: 'internal-nacl-plugin', description: '' }
  ]);
  defineGetter(Navigator.prototype, 'mimeTypes', () => [
    { type: 'application/pdf', suffixes: 'pdf', description: 'Portable Document Format' }
  ]);
  defineGetter(navigator, 'mimeTypes', () => [
    { type: 'application/pdf', suffixes: 'pdf', description: 'Portable Document Format' }
  ]);

  try {
    if (!window.chrome) {
      Object.defineProperty(window, 'chrome', {
        value: { runtime: {} },
        configurable: true
      });
    } else if (!window.chrome.runtime) {
      Object.defineProperty(window.chrome, 'runtime', { value: {}, configurable: true });
    }
  } catch (_) {}

  const originalQuery = window.navigator.permissions && window.navigator.permissions.query;
  if (originalQuery) {
    window.navigator.permissions.query = (parameters) => {
      if (parameters && parameters.name === 'notifications') {
        return Promise.resolve({ state: Notification.permission });
      }
      return originalQuery.call(window.navigator.permissions, parameters);
    };
  }

  const patchWebGL = (prototype) => {
    if (!prototype || !prototype.getParameter) return;
    const originalGetParameter = prototype.getParameter;
    prototype.getParameter = function(parameter) {
      if (parameter === 37445) return 'Intel Inc.';
      if (parameter === 37446) return 'Intel Iris OpenGL Engine';
      return originalGetParameter.call(this, parameter);
    };
  };

  patchWebGL(window.WebGLRenderingContext && window.WebGLRenderingContext.prototype);
  patchWebGL(window.WebGL2RenderingContext && WebGL2RenderingContext.prototype);
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stealth_args_strip_headless_user_agent_marker() {
        let profile = StealthProfile::new(
            true,
            Some("Mozilla/5.0 HeadlessChrome/124.0.0.0".to_owned()),
        );
        let args = profile.chrome_args();

        assert!(args.iter().any(|arg| arg.contains("AutomationControlled")));
        assert!(args.iter().any(|arg| arg.contains("Chrome/124.0.0.0")));
        assert!(!args.iter().any(|arg| arg.contains("HeadlessChrome")));
        assert!(!args.iter().any(|arg| arg.contains("--enable-automation")));
    }

    #[test]
    fn injected_script_covers_baseline_checks() {
        assert!(BASELINE_SCRIPT.contains("webdriver"));
        assert!(BASELINE_SCRIPT.contains("plugins"));
        assert!(BASELINE_SCRIPT.contains("mimeTypes"));
        assert!(BASELINE_SCRIPT.contains("WebGLRenderingContext"));
    }
}
