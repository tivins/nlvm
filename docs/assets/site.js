/* nlvm site — NL syntax highlighting, terminal animation, scroll reveal */
(function () {
  "use strict";

  /* ---------- NL syntax highlighter ---------- */

  var KEYWORDS = new Set([
    "namespace", "use", "class", "interface", "enum",
    "public", "private", "protected", "static", "readonly",
    "extends", "implements", "construct", "destruct",
    "if", "else", "while", "for", "break", "continue", "return", "match", "default",
    "try", "catch", "finally", "throw", "throws",
    "new", "this", "super", "instanceof", "ref",
    "auto", "void", "int", "float", "bool", "string",
    "null", "true", "false"
  ]);

  function escapeHtml(s) {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }

  // Tokenizes raw NL source and returns highlighted HTML.
  var NL_TOKEN = /(\/\/[^\n]*|\/\*[\s\S]*?\*\/)|("(?:[^"\\]|\\.)*")|\b(\d+(?:\.\d+)?)\b|\b([A-Za-z_][A-Za-z0-9_]*)\b|(=>|\?\?|\?:|[|])/g;

  function highlightNl(src) {
    var html = "";
    var last = 0;
    var m;
    NL_TOKEN.lastIndex = 0;
    while ((m = NL_TOKEN.exec(src)) !== null) {
      html += escapeHtml(src.slice(last, m.index));
      last = NL_TOKEN.lastIndex;
      var text = escapeHtml(m[0]);
      if (m[1]) {
        html += '<span class="tok-com">' + text + "</span>";
      } else if (m[2]) {
        html += '<span class="tok-str">' + text + "</span>";
      } else if (m[3]) {
        html += '<span class="tok-num">' + text + "</span>";
      } else if (m[4]) {
        if (KEYWORDS.has(m[4])) {
          html += '<span class="tok-kw">' + text + "</span>";
        } else if (/^[A-Z]/.test(m[4])) {
          html += '<span class="tok-type">' + text + "</span>";
        } else {
          html += text;
        }
      } else if (m[5]) {
        html += '<span class="tok-punct">' + text + "</span>";
      }
    }
    html += escapeHtml(src.slice(last));
    return html;
  }

  // Shell blocks: color "$ " prompts and comment lines; leave output dim.
  function highlightSh(src) {
    return src.split("\n").map(function (line) {
      if (/^\$\s/.test(line)) {
        return '<span class="prompt">$</span> ' + escapeHtml(line.slice(2));
      }
      if (/^#/.test(line)) {
        return '<span class="tok-com">' + escapeHtml(line) + "</span>";
      }
      return '<span class="out">' + escapeHtml(line) + "</span>";
    }).join("\n");
  }

  document.querySelectorAll("pre > code").forEach(function (code) {
    var pre = code.parentElement;
    var raw = code.textContent.replace(/^\n+|\s+$/g, "");
    if (pre.classList.contains("nl")) {
      code.innerHTML = highlightNl(raw);
    } else if (pre.classList.contains("sh")) {
      code.innerHTML = highlightSh(raw);
    } else {
      code.textContent = raw;
    }

    var btn = document.createElement("button");
    btn.className = "copy-btn";
    btn.type = "button";
    btn.textContent = "copy";
    btn.addEventListener("click", function () {
      var text = pre.classList.contains("sh")
        ? raw.split("\n").filter(function (l) { return /^\$\s/.test(l); })
             .map(function (l) { return l.slice(2); }).join("\n") || raw
        : raw;
      navigator.clipboard.writeText(text).then(function () {
        btn.textContent = "copied";
        setTimeout(function () { btn.textContent = "copy"; }, 1400);
      });
    });
    pre.appendChild(btn);
  });

  /* ---------- scroll reveal ---------- */

  var revealed = document.querySelectorAll(".reveal");
  if ("IntersectionObserver" in window) {
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (e.isIntersecting) {
          e.target.classList.add("visible");
          io.unobserve(e.target);
        }
      });
    }, { threshold: 0.12 });
    revealed.forEach(function (el) { io.observe(el); });
  } else {
    revealed.forEach(function (el) { el.classList.add("visible"); });
  }

  /* ---------- animated terminal ---------- */

  var term = document.getElementById("terminal-demo");
  if (!term) return;

  // Looping scenarios. A step is a typed comment ("note"), a typed command
  // ("cmd"), or printed output lines ("out": [cssClass, text] pairs).
  // Every command and its output was captured from the real toolchain.
  var SCENARIOS = [
    {
      steps: [
        { note: "# a whole source tree, one shippable file" },
        { cmd: "nlc src/ -o software.nlp" },
        { cmd: "nlvm software.nlp" },
        { out: [["ok", "Hello, world!"]] }
      ]
    },
    {
      steps: [
        { note: "# null bugs don't compile" },
        { cmd: "nlc Bad.nl -o bad.nlp" },
        { out: [["err", "Error: Bad.nl:4: E003 — Cannot assign 'null' to type 'string' (type does not include null)"]] },
        { note: "# neither do missed cases" },
        { cmd: "nlc match/ -o app.nlp" },
        { out: [["err", "Error: match/Match.nl:6: E047 — Match expression is not exhaustive (missing 'default')"]] }
      ]
    },
    {
      steps: [
        { note: "# every exception knows where it came from" },
        { cmd: "nlvm crash.nlp" },
        { out: [
          ["err", "Unhandled exception: ArithmeticException: division by zero"],
          ["out", "    at app/Main.nl:4"],
          ["out", "    at app/Main.nl:7"]
        ] }
      ]
    },
    {
      steps: [
        { note: "# one toolchain, one versioned spec" },
        { cmd: "nlc --version" },
        { out: [["out", "nlc 0.5.1 (nlvm-specs 0.8.44)"]] },
        { cmd: "nltest tests/" },
        { out: [["ok", "140 passed, 0 failed, 140 total"]] }
      ]
    }
  ];

  var reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  var cursor = document.createElement("span");
  cursor.className = "cursor";

  function renderScenarioInstant(scenario) {
    var html = "";
    scenario.steps.forEach(function (step) {
      if (step.note) {
        html += '<span class="com">' + escapeHtml(step.note) + "</span>\n";
      } else if (step.cmd) {
        html += '<span class="prompt">$</span> ' + escapeHtml(step.cmd) + "\n";
      } else {
        step.out.forEach(function (l) {
          html += '<span class="' + l[0] + '">' + escapeHtml(l[1]) + "</span>\n";
        });
      }
    });
    term.innerHTML = html;
  }

  if (reduced) {
    renderScenarioInstant(SCENARIOS[0]);
    return;
  }

  var started = false;
  function startTerminal() {
    if (started) return;
    started = true;
    term.innerHTML = "";
    term.appendChild(cursor);
    runStep(0, 0);
  }

  function appendLine(cls, text) {
    var span = document.createElement("span");
    span.className = cls;
    span.textContent = text;
    term.insertBefore(span, cursor);
    term.insertBefore(document.createTextNode("\n"), cursor);
  }

  function runStep(s, i) {
    var steps = SCENARIOS[s].steps;
    if (i >= steps.length) {
      // Scenario done: hold, fade, then start the next one.
      setTimeout(function () {
        term.classList.add("fade-out");
        setTimeout(function () {
          term.innerHTML = "";
          term.appendChild(cursor);
          term.classList.remove("fade-out");
          runStep((s + 1) % SCENARIOS.length, 0);
        }, 350);
      }, 3200);
      return;
    }
    var step = steps[i];
    if (step.note) {
      var note = document.createElement("span");
      note.className = "com";
      term.insertBefore(note, cursor);
      typeInto(note, step.note, 0, function () {
        term.insertBefore(document.createTextNode("\n"), cursor);
        setTimeout(function () { runStep(s, i + 1); }, 200);
      });
    } else if (step.cmd) {
      var prompt = document.createElement("span");
      prompt.className = "prompt";
      prompt.textContent = "$ ";
      term.insertBefore(prompt, cursor);
      var cmd = document.createElement("span");
      term.insertBefore(cmd, cursor);
      typeInto(cmd, step.cmd, 0, function () {
        term.insertBefore(document.createTextNode("\n"), cursor);
        setTimeout(function () { runStep(s, i + 1); }, 250);
      });
    } else {
      step.out.forEach(function (l) { appendLine(l[0], l[1]); });
      setTimeout(function () { runStep(s, i + 1); }, 500);
    }
  }

  function typeInto(el, text, pos, done) {
    if (pos >= text.length) { done(); return; }
    el.textContent += text[pos];
    setTimeout(function () { typeInto(el, text, pos + 1, done); }, 24 + Math.random() * 36);
  }

  if ("IntersectionObserver" in window) {
    var tio = new IntersectionObserver(function (entries) {
      if (entries.some(function (e) { return e.isIntersecting; })) {
        tio.disconnect();
        setTimeout(startTerminal, 350);
      }
    }, { threshold: 0.4 });
    tio.observe(term);
  } else {
    startTerminal();
  }
})();
