const express = require("express");
const { chromium } = require("playwright");
const cors = require("cors");

const app = express();
const port = Number(process.env.PLAYWRIGHT_PORT || 4000);

const SELECTOR_TIMEOUT = 15000;
const SEARCH_SELECTOR_TIMEOUT = 3000;
const OPERATION_RETRIES = 3;
const BACKOFF_BASE_MS = 1000;

const SEARCH_INPUT_SELECTORS = [
  "textarea[name=\"q\"]",
  "input[name=\"q\"]",
  "input[type=\"search\"]",
  "textarea[title=\"Search\"]",
  "input[title=\"Search\"]",
  "#searchbox input",
  "#search input",
];
const DEFAULT_EXTRACT_SELECTOR = "p";

app.use(express.json());
app.use(cors());

let browser;
let context;
let page;

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function createStructuredError(type, message, action = "retry", statusCode = 500, details) {
  const error = new Error(message);
  error.type = type;
  error.action = action;
  error.statusCode = statusCode;
  error.details = details;
  return error;
}

function inferErrorType(error) {
  const message = String(error?.message || error || "").toLowerCase();

  if (error?.type) {
    return error.type;
  }
  if (message.includes("waitforselector") || message.includes("timeout")) {
    return message.includes("navigation") ? "navigation_timeout" : "selector_timeout";
  }
  if (
    message.includes("net::err") ||
    message.includes("network") ||
    message.includes("econnreset") ||
    message.includes("socket hang up")
  ) {
    return "network_error";
  }
  if (message.includes("unknown action")) {
    return "unsupported_action";
  }
  if (message.includes("required")) {
    return "validation_error";
  }

  return "playwright_error";
}

function normalizeError(error) {
  if (error?.status === "error") {
    return {
      status: "error",
      type: error.type || inferErrorType(error),
      message: error.message || "Unknown Playwright server error",
      action: error.action || "retry",
      statusCode: error.statusCode || 500,
    };
  }

  return {
    status: "error",
    type: inferErrorType(error),
    message: error?.message || String(error),
    action: error?.action || (isRetryableError(error) ? "retry" : "abort"),
    statusCode: error?.statusCode || 500,
  };
}

function isRetryableError(error) {
  const type = error?.type || inferErrorType(error);
  return type === "selector_timeout" || type === "navigation_timeout" || type === "network_error";
}

async function retryOperation(fn, retries = OPERATION_RETRIES, label = "operation") {
  let lastError;

  for (let attempt = 1; attempt <= retries; attempt += 1) {
    try {
      return await fn();
    } catch (error) {
      const normalized = normalizeError(error);
      lastError = normalized;

      if (!isRetryableError(normalized) || attempt === retries) {
        throw createStructuredError(
          normalized.type,
          normalized.message,
          normalized.action,
          normalized.statusCode,
          normalized.details
        );
      }

      const delay = BACKOFF_BASE_MS * attempt;
      console.warn(`RETRY ${attempt}/${retries}: ${label} -> ${normalized.message}`);
      await sleep(delay);
    }
  }

  throw createStructuredError(
    lastError?.type || "playwright_error",
    lastError?.message || `Failed ${label}`,
    lastError?.action || "retry",
    lastError?.statusCode || 500
  );
}

async function waitForPageStability(activePage, { allowFallback = true } = {}) {
  await activePage.waitForLoadState("domcontentloaded", { timeout: SELECTOR_TIMEOUT });

  try {
    await activePage.waitForLoadState("networkidle", { timeout: SELECTOR_TIMEOUT });
  } catch (error) {
    if (!allowFallback || inferErrorType(error) !== "navigation_timeout") {
      throw error;
    }

    console.warn("NETWORKIDLE timeout, falling back to load state");
    await activePage.waitForLoadState("load", { timeout: SELECTOR_TIMEOUT });
  }
}

async function waitForPostActionStability(activePage) {
  try {
    await activePage.waitForLoadState("networkidle", { timeout: 5000 });
  } catch (error) {
    if (isRetryableError(error)) {
      console.warn(`POST ACTION STABILITY WARNING: ${error.message}`);
      return;
    }

    throw error;
  }
}

async function ensurePage() {
  if (!browser || !browser.isConnected()) {
    browser = await chromium.launch({ headless: false });
  }

  if (!context) {
    context = await browser.newContext({
      viewport: { width: 1280, height: 800 },
      userAgent:
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36",
    });
  }

  if (!page || page.isClosed()) {
    page = await context.newPage();
    page.setDefaultTimeout(SELECTOR_TIMEOUT);
    page.setDefaultNavigationTimeout(SELECTOR_TIMEOUT);
  }

  return page;
}

async function safeWaitForSelector(activePage, selector) {
  console.log("WAITING FOR:", selector);

  try {
    return await retryOperation(
      async () =>
        activePage.waitForSelector(selector, {
          timeout: SELECTOR_TIMEOUT,
          state: "visible",
        }),
      OPERATION_RETRIES,
      `selector ${selector}`
    );
  } catch (error) {
    const normalized = normalizeError(error);
    throw createStructuredError(
      "selector_timeout",
      `Timed out waiting for selector: ${selector}. ${normalized.message}`,
      "retry"
    );
  }
}

async function extractVisibleText(activePage, selector) {
  console.log("EXTRACT:", selector);

  const data = await retryOperation(
    async () =>
      activePage.$$eval(selector, (elements) =>
        elements
          .map((element) => element.innerText.trim())
          .filter(Boolean)
      ),
    OPERATION_RETRIES,
    `extract ${selector}`
  );

  console.log("RESULT LENGTH:", data.length);

  if (!Array.isArray(data) || data.length === 0) {
    throw createStructuredError(
      "extract_failed",
      `Selector not found or produced no readable text: ${selector}`,
      "retry",
      404
    );
  }

  return data;
}

async function extractPageText(activePage) {
  console.log("EXTRACT_TEXT");

  const data = await retryOperation(
    async () =>
      activePage.evaluate(() => (document.body ? document.body.innerText : "")),
    OPERATION_RETRIES,
    "extract_text"
  );

  const trimmed = String(data || "").trim().slice(0, 5000);
  console.log("RESULT LENGTH:", trimmed.length);

  if (!trimmed) {
    throw createStructuredError(
      "extract_failed",
      "Page did not contain readable text",
      "retry",
      404
    );
  }

  return trimmed;
}

async function findSearchInput(activePage) {
  for (const selector of SEARCH_INPUT_SELECTORS) {
    try {
      console.log("WAITING FOR:", selector);
      const locator = activePage.locator(selector).first();
      await locator.waitFor({
        timeout: SEARCH_SELECTOR_TIMEOUT,
        state: "visible",
      });
      return { selector, locator };
    } catch (_) {
      // Try the next selector.
    }
  }

  throw createStructuredError(
    "selector_timeout",
    `Unable to locate a visible search input. Tried selectors: ${SEARCH_INPUT_SELECTORS.join(", ")}`,
    "retry"
  );
}

function validateField(value, field, action) {
  if (!value) {
    throw createStructuredError(
      "validation_error",
      `${field} is required for ${action}`,
      "fix_request",
      400
    );
  }
}

app.post("/playwright/action", async (req, res) => {
  const { action, url, query, selector, text, key } = req.body;
  console.log(`RECEIVED ACTION: ${action}`, req.body);

  try {
    validateField(action, "action", "request");

    const activePage = await ensurePage();

    switch (action) {
      case "open_url":
        validateField(url, "url", action);
        console.log("NAVIGATING:", url);
        await retryOperation(async () => {
          await activePage.goto(url, {
            waitUntil: "domcontentloaded",
            timeout: SELECTOR_TIMEOUT,
          });
          await waitForPageStability(activePage);
        }, OPERATION_RETRIES, `open_url ${url}`);
        return res.json({ status: "ok", action });

      case "search": {
        validateField(query, "query", action);
        await waitForPageStability(activePage);

        const searchTarget = selector
          ? { selector, locator: activePage.locator(selector).first() }
          : await findSearchInput(activePage);

        await safeWaitForSelector(activePage, searchTarget.selector);
        console.log("TYPE:", query);
        await retryOperation(async () => {
          await searchTarget.locator.click();
          await searchTarget.locator.fill(query);
          await activePage.keyboard.press("Enter");
          await waitForPostActionStability(activePage);
        }, OPERATION_RETRIES, `search ${searchTarget.selector}`);

        return res.json({ status: "ok", action, selector: searchTarget.selector });
      }

      case "click":
        validateField(selector, "selector", action);
        await waitForPageStability(activePage);
        await retryOperation(async () => {
          const element = await safeWaitForSelector(activePage, selector);
          console.log("CLICK:", selector);
          await element.click();
          await waitForPostActionStability(activePage);
        }, OPERATION_RETRIES, `click ${selector}`);
        return res.json({ status: "ok", action, selector });

      case "type":
        validateField(selector, "selector", action);
        validateField(text, "text", action);
        await waitForPageStability(activePage);
        await retryOperation(async () => {
          await safeWaitForSelector(activePage, selector);
          console.log("TYPE:", text);
          await activePage.fill(selector, text, { timeout: SELECTOR_TIMEOUT });
          await waitForPostActionStability(activePage);
        }, OPERATION_RETRIES, `type ${selector}`);
        return res.json({ status: "ok", action, selector });

      case "press": {
        const keyToPress = key || text;
        validateField(keyToPress, "key", action);
        await waitForPageStability(activePage);
        await retryOperation(async () => {
          if (selector) {
            await safeWaitForSelector(activePage, selector);
            await activePage.focus(selector);
          }
          console.log("PRESS:", keyToPress);
          await activePage.keyboard.press(keyToPress);
          await waitForPostActionStability(activePage);
        }, OPERATION_RETRIES, `press ${keyToPress}`);
        return res.json({ status: "ok", action, key: keyToPress });
      }

      case "screenshot": {
        await waitForPageStability(activePage);
        const screenshot = await retryOperation(
          async () => activePage.screenshot({ encoding: "base64" }),
          OPERATION_RETRIES,
          "screenshot"
        );
        return res.json({
          status: "ok",
          action,
          screenshot: `data:image/png;base64,${screenshot}`,
        });
      }

      case "extract": {
        const extractSelector = selector || DEFAULT_EXTRACT_SELECTOR;
        await waitForPageStability(activePage);
        try {
          await safeWaitForSelector(activePage, extractSelector);
        } catch (error) {
          throw createStructuredError(
            "extract_failed",
            `Selector not found: ${extractSelector}`,
            "retry",
            404
          );
        }
        return res.json({
          status: "ok",
          action,
          selector: extractSelector,
          data: await extractVisibleText(activePage, extractSelector),
        });
      }

      case "extract_text":
        await waitForPageStability(activePage);
        return res.json({
          status: "ok",
          action,
          data: await extractPageText(activePage),
        });

      default:
        throw createStructuredError(
          "unsupported_action",
          `Unknown action: ${action}`,
          "abort",
          400
        );
    }
  } catch (error) {
    const normalized = normalizeError(error);
    console.error("PLAYWRIGHT ACTION ERROR:", normalized, error);
    return res.status(normalized.statusCode).json({
      status: "error",
      type: normalized.type,
      message: normalized.message,
      action: normalized.action,
    });
  }
});

app.get("/health", (req, res) => {
  res.json({ status: "ok", browser: Boolean(browser && browser.isConnected()) });
});

app.listen(port, "127.0.0.1", () => {
  console.log(`Playwright server listening at http://127.0.0.1:${port}`);
});
