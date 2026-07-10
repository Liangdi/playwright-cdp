// Minimal selector engine injected into each page execution context.
//
// Contract (registered on `self.__pwcdpInjected`):
//   parseSelector(selector)            -> { engine, body }
//   querySelectorAll(root, selector)   -> Element[]
//   querySelector(root, selector)      -> Element | null
//   elementState(element)              -> { visible, enabled, editable, checked, stable }
//
// This is a STAND-IN used so the Rust skeleton is runnable end-to-end. It
// supports CSS, `text=`, `xpath=`, `role=` (best-effort) and ` >> ` chaining.
// The full Playwright selector-engine bundle (Apache-2.0) drops in here with
// the same `__pwcdpInjected` contract — see THIRD_PARTY.md and the `xtask`
// build script.
(function () {
  if (self.__pwcdpInjected) return;

  var IMPLICIT_ROLE = {
    button: ['button'],
    link: ['a'],
    textbox: ['input[type="text"]', 'input[type="email"]', 'input[type="password"]',
              'input[type="search"]', 'input[type="tel"]', 'input[type="url"]',
              'input:not([type])', 'textarea'],
    checkbox: ['input[type="checkbox"]'],
    radio: ['input[type="radio"]'],
    searchbox: ['input[type="search"]'],
    heading: ['h1', 'h2', 'h3', 'h4', 'h5', 'h6'],
    image: ['img'],
    img: ['img'],
    list: ['ul', 'ol'],
    listitem: ['li'],
    navigation: ['nav'],
    main: ['main'],
    article: ['article'],
    section: ['section'],
    form: ['form'],
    combobox: ['select'],
    paragraph: ['p'],
    banner: ['header'],
    contentinfo: ['footer'],
    figure: ['figure'],
  };

  function normalizeText(s) {
    return (s || '').replace(/\s+/g, ' ').trim();
  }

  function parseSelector(selector) {
    selector = String(selector);
    var engine = 'css', body = selector;
    if (selector.indexOf('text=') === 0) { engine = 'text'; body = selector.slice(5); }
    else if (selector.indexOf('text"=') === 0) { engine = 'text'; body = selector.slice(6); } // exact variant
    else if (selector.indexOf('xpath=') === 0) { engine = 'xpath'; body = selector.slice(6); }
    else if (selector.indexOf('//') === 0 || selector.indexOf('(/') === 0) { engine = 'xpath'; body = selector; }
    else if (selector.indexOf('role=') === 0) { engine = 'role'; body = selector.slice(5); }
    return { engine: engine, body: body };
  }

  function queryStep(root, step) {
    var parsed = parseSelector(step);
    var out = [];
    if (parsed.engine === 'css') {
      try { out = Array.prototype.slice.call(root.querySelectorAll(parsed.body)); } catch (e) { out = []; }
    } else if (parsed.engine === 'xpath') {
      var res = (root.ownerDocument || root).evaluate(parsed.body, root, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
      for (var i = 0; i < res.snapshotLength; i++) {
        var n = res.snapshotItem(i);
        if (n && n.nodeType === 1) out.push(n);
      }
    } else if (parsed.engine === 'text') {
      var term = normalizeText(parsed.body);
      var all = root.querySelectorAll('*');
      var matched = [];
      for (var j = 0; j < all.length; j++) {
        var el = all[j];
        if (normalizeText(el.textContent).toLowerCase().indexOf(term.toLowerCase()) !== -1) {
          matched.push(el);
        }
      }
      // Prefer innermost matches: drop elements that contain another match.
      var set = new Set(matched);
      matched.forEach(function (el) {
        var inner = el.querySelectorAll('*');
        for (var k = 0; k < inner.length; k++) {
          if (set.has(inner[k])) { set.delete(el); break; }
        }
      });
      out = Array.from(set);
    } else if (parsed.engine === 'role') {
      out = queryRole(root, parsed.body);
    }
    return out;
  }

  function queryRole(root, body) {
    // role=Name[attrs...]  -- parse role + bracketed name
    var m = body.match(/^([^\[\]]+)(?:\[(.*)\])?$/);
    if (!m) return [];
    var role = normalizeText(m[1]).toLowerCase();
    var attrs = parseAttrs(m[2] || '');
    var selectors = IMPLICIT_ROLE[role] || [];
    var candidates = [];
    // explicit role attribute
    try { candidates = candidates.concat(Array.prototype.slice.call(root.querySelectorAll('[role="' + role + '"]'))); } catch (e) {}
    selectors.forEach(function (sel) {
      try { candidates = candidates.concat(Array.prototype.slice.call(root.querySelectorAll(sel))); } catch (e) {}
    });
    if (attrs.name) {
      candidates = candidates.filter(function (el) {
        var name = normalizeText(el.getAttribute('aria-label') || el.textContent || '');
        return attrs.exact ? name === attrs.name : name.toLowerCase().indexOf(attrs.name.toLowerCase()) !== -1;
      });
    }
    // dedupe preserving order
    var seen = new Set();
    return candidates.filter(function (el) { if (seen.has(el)) return false; seen.add(el); return true; });
  }

  function parseAttrs(s) {
    var attrs = {};
    if (!s) return attrs;
    var re = /\s*(\w+)\s*=\s*"([^"]*)"\s*/g;
    var mm;
    while ((mm = re.exec(s)) !== null) {
      attrs[mm[1]] = mm[2];
    }
    attrs.exact = attrs.exact === 'true';
    return attrs;
  }

  function querySelectorAll(root, selector) {
    var steps = String(selector).split(/\s*>>\s*/);
    var current = [root];
    for (var s = 0; s < steps.length; s++) {
      var next = [];
      for (var c = 0; c < current.length; c++) {
        var got = queryStep(current[c], steps[s]);
        for (var g = 0; g < got.length; g++) next.push(got[g]);
      }
      current = next;
      if (current.length === 0) break;
    }
    var seen = new Set();
    var result = [];
    for (var i = 0; i < current.length; i++) {
      var el = current[i];
      if (el && el.nodeType === 1 && !seen.has(el)) { seen.add(el); result.push(el); }
    }
    return result;
  }

  function isVisible(el) {
    if (!el || !el.getBoundingClientRect) return false;
    var style = (el.ownerDocument && el.ownerDocument.defaultView)
      ? el.ownerDocument.defaultView.getComputedStyle(el) : null;
    if (!style) return false;
    if (style.visibility === 'hidden' || style.display === 'none') return false;
    var rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  }

  function elementState(el) {
    var disabled = el.disabled === true;
    return {
      visible: isVisible(el),
      enabled: !disabled,
      editable: !disabled && (el.isContentEditable || /^(input|textarea|select)$/.test(el.tagName.toLowerCase())),
      checked: el.checked === true,
      stable: true,
    };
  }

  self.__pwcdpInjected = {
    parseSelector: parseSelector,
    querySelectorAll: querySelectorAll,
    querySelector: function (root, selector) {
      var all = querySelectorAll(root, selector);
      return all.length ? all[0] : null;
    },
    elementState: elementState,
    isVisible: isVisible,
  };
})();
