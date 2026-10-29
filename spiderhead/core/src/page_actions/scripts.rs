// Browser-side scripts. They run in the page, so the text is kept byte-for-byte.

/// Pre-click safety probe: counts matches and refuses form, link or navigation targets.
pub const CLICK_PROBE_JS: &str = r#"arg => {
                      const matches = document.querySelectorAll(arg.selector);
                      const element = matches[0];
                      if (!element) return {count: 0, safe: false};
                      const tag = element.tagName.toLowerCase();
                      const role = (element.getAttribute('role') || '').toLowerCase();
                      const type = (element.getAttribute('type') || '').toLowerCase();
                      const link = element.closest('a[href], [role="link"]');
                      const control = element.closest('button, input');
                      const controlType = control
                        ? (control.getAttribute('type') || '').toLowerCase()
                        : '';
                      const unsafe = element.closest('form') || element.form || element.href
                        || element.hasAttribute('formaction') || element.hasAttribute('formmethod')
                        || element.hasAttribute('href') || link || role === 'link' || tag === 'a'
                        || tag === 'input' || type === 'submit' || type === 'reset'
                        || (control && (control.tagName.toLowerCase() === 'input'
                          || controlType === 'submit' || controlType === 'reset'
                          || control.hasAttribute('formaction')));
                      return {count: matches.length, safe: !unsafe};
                    }"#;
/// Extracts text/href/value of up to `max_items` matches within the output budget.
pub const EXTRACT_JS: &str = r#"arg => {
                      const nodes = document.querySelectorAll(arg.selector);
                      const values = [];
                      let chars = 0;
                      for (const node of Array.from(nodes).slice(0, arg.max_items)) {
                        const raw = arg.attribute === 'text'
                          ? (node.innerText || node.textContent || '')
                          : (node.getAttribute(arg.attribute) || '');
                        const value = String(raw).slice(0, arg.per_value_chars);
                        if (chars + value.length > arg.max_total_chars) break;
                        values.push(value);
                        chars += value.length;
                      }
                      return {values, omitted: Math.max(0, nodes.length - values.length)};
                    }"#;
/// Reads the bounded tail of `window.dataLayer` as plain JSON-like data.
pub const DATALAYER_JS: &str = r#"arg => {
                      const source = window.dataLayer;
                      if (!Array.isArray(source)) return {items: null, truncated: false};
                      let chars = 0;
                      let nodes = 0;
                      const seen = new WeakSet();
                      const clone = (value, depth) => {
                        nodes += 1;
                        if (nodes > arg.max_nodes || chars >= arg.max_chars) return null;
                        if (value === null || typeof value === 'boolean') return value;
                        if (typeof value === 'number') {
                          chars += String(value).length;
                          return Number.isFinite(value) ? value : null;
                        }
                        if (typeof value === 'string') {
                          const clipped = value.slice(0, arg.max_string_chars);
                          chars += clipped.length;
                          return clipped;
                        }
                        if (depth >= arg.max_depth || typeof value !== 'object' || seen.has(value)) return null;
                        seen.add(value);
                        const isArray = Array.isArray(value);
                        const descriptors = Object.getOwnPropertyDescriptors(value);
                        const out = isArray ? [] : Object.create(null);
                        for (const key of Object.keys(descriptors).slice(0, arg.max_keys)) {
                          if (isArray && key === 'length') continue;
                          const descriptor = descriptors[key];
                          if (!Object.prototype.hasOwnProperty.call(descriptor, 'value')) continue;
                          if (chars >= arg.max_chars) break;
                          chars += Math.min(key.length, arg.max_key_chars);
                          const item = clone(descriptor.value, depth + 1);
                          if (isArray) {
                            const index = Number(key);
                            if (Number.isInteger(index) && index >= 0) out[index] = item;
                          } else out[key.slice(0, arg.max_key_chars)] = item;
                        }
                        seen.delete(value);
                        return out;
                      };
                      const items = [];
                      for (const item of source.slice(-arg.max_items)) {
                        if (chars >= arg.max_chars || nodes >= arg.max_nodes) break;
                        items.push(clone(item, 0));
                      }
                      return {items, truncated: items.length < source.length || chars >= arg.max_chars || nodes >= arg.max_nodes};
                    }"#;
/// Scroll script; the argument is the pixel count.
pub const SCROLL_JS: &str = r#"pixels => window.scrollBy(0, pixels)"#;
