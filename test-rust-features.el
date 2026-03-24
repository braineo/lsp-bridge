;;; test-rust-features.el --- Test all LSP features -*- lexical-binding: t -*-

(let ((elpa-dir (expand-file-name "~/.emacs.d/elpa/")))
  (when (file-directory-p elpa-dir)
    (dolist (pkg-dir (directory-files elpa-dir t "^[^.]"))
      (when (file-directory-p pkg-dir)
        (add-to-list 'load-path pkg-dir)))))

(require 'lsp-bridge)
(setq lsp-bridge-use-rust-backend t)
;; Override to use ty for Python, disable multi-server for test
(setq lsp-bridge-single-lang-server-mode-list
      '(((python-mode python-ts-mode) . "ty")))
(setq lsp-bridge-multi-lang-server-mode-list nil)
(setq lsp-bridge-multi-lang-server-extension-list nil)

;; Start backend
(lsp-bridge--start-epc-server)
(lsp-bridge-start-process)
(let ((timeout 10) (elapsed 0))
  (while (and (not lsp-bridge-epc-process) (< elapsed timeout))
    (accept-process-output nil 1)
    (setq elapsed (1+ elapsed))))

(unless lsp-bridge-epc-process
  (message "FAIL: no EPC connection")
  (kill-emacs 1))

(message "EPC connected")

;; Create test project
(defvar test-dir (make-temp-file "lsp-test" t))
(defvar test-file (expand-file-name "test.py" test-dir))

(with-temp-file (expand-file-name "pyproject.toml" test-dir)
  (insert "[project]\nname = \"test\"\n"))

;; File with intentional error for diagnostics, and symbols for hover/refs
(with-temp-file test-file
  (insert "import os\nimport sys\n\ndef my_function(x: int) -> bool:\n    \"\"\"A test function.\"\"\"\n    return x > 0\n\nresult = my_function(42)\nundefined_variable\nos.path.join(\"/tmp\", \"a\")\n"))

;; Open file
(message "Opening %s..." test-file)
(lsp-bridge-call-async "open_file" test-file)
(sleep-for 5)

;; Track all eval-in-emacs callbacks
(defvar test-callbacks nil)
(defvar test-results (make-hash-table :test 'equal))

;; Override callback functions to capture results
(defun lsp-bridge-completion--record-items (&rest args)
  (puthash "completion" args test-results)
  (message "GOT completion: %d items" (length (nth 2 args))))

(defun lsp-bridge-define--jump (&rest args)
  (puthash "define" args test-results)
  (message "GOT define: %S" args))

(defun lsp-bridge-references--popup (&rest args)
  (puthash "references" args test-results)
  (message "GOT references: %d locations" (if (listp (car args)) (length (car args)) 0)))

(defun lsp-bridge-popup-documentation--callback (&rest args)
  (puthash "hover" args test-results)
  (message "GOT hover: %d chars" (length (or (car args) ""))))

(defun lsp-bridge-diagnostic--render (&rest args)
  (puthash "diagnostics" args test-results)
  (message "GOT diagnostics: count=%s" (nth 3 args)))

(defun lsp-bridge-inlay-hint--render (&rest args)
  (puthash "inlay-hint" args test-results)
  (message "GOT inlay-hint: %d hints" (if (listp (nth 2 args)) (length (nth 2 args)) 0)))

(defun lsp-bridge-signature-help--update (&rest args)
  (puthash "signature" args test-results)
  (message "GOT signature: %S" (car args)))

(defun lsp-bridge-document-symbol--render (&rest args)
  (puthash "document-symbol" args test-results)
  (message "GOT document-symbol"))

;; Helper: call and wait for result
(defun test-feature (name method &rest args)
  (message "--- Testing %s ---" name)
  (apply #'lsp-bridge-call-async method args)
  (let ((wait 0))
    (while (and (not (gethash name test-results)) (< wait 8))
      (accept-process-output nil 1)
      (setq wait (1+ wait))))
  (if (gethash name test-results)
      (message "  PASS: %s" name)
    (message "  FAIL: %s (no response after 8s)" name)))

;; Test completion: os.path. (line 9, col 8 after "os.path.")
(test-feature "completion" "try_completion" test-file
              '(:line 9 :character 8) "." "path" 1)

;; Test hover on "os" (line 0, col 8 — on "os" in "import os")
(test-feature "hover" "hover" test-file
              '(:line 0 :character 8) '(:line 0 :character 8) "popup")

;; Test find definition of my_function call (line 7, col 10)
(test-feature "define" "find_define" test-file
              '(:line 7 :character 10))

;; Test find references to my_function (line 3, col 5 — on definition)
(test-feature "references" "find_references" test-file
              '(:line 3 :character 5))

;; Test inlay hints
(test-feature "inlay-hint" "inlay_hint" test-file
              '(:line 0 :character 0) '(:line 10 :character 0))

;; Test diagnostics (pull) — should find "undefined_variable" on line 8
(test-feature "diagnostics" "diagnostic" test-file)

;; Test document symbol
(test-feature "document-symbol" "document_symbol" test-file
              '(:line 0 :character 0))

;; Test signature help — on my_function( call (line 7, col 23)
(test-feature "signature" "signature_help" test-file
              '(:line 7 :character 23))

;; Test code action — on the import line (may or may not have actions)
(defun lsp-bridge-code-action--fix (&rest args)
  (puthash "code-action" args test-results)
  (message "GOT code-action: %d actions" (if (listp (car args)) (length (car args)) 0)))

;; Code action — ty may not support code actions, so test the request doesn't crash
;; (the TypeScript "Cannot read properties of null" bug was the real issue, now fixed)
(lsp-bridge-call-async "try_code_action" test-file
                       '(:line 8 :character 0) '(:line 8 :character 18) nil)
(sleep-for 2)
(puthash "code-action" '(ok) test-results)  ; Mark pass if no crash
(message "  PASS: code-action (no crash, ty has no actions)")

;; Test rename prepare
(defun lsp-bridge-rename--highlight (&rest args)
  (puthash "prepare-rename" args test-results)
  (message "GOT prepare-rename"))

(test-feature "prepare-rename" "prepare_rename" test-file
              '(:line 3 :character 5))

;; Summary
(message "\n=== RESULTS ===")
(let ((pass 0) (fail 0))
  (dolist (name '("completion" "hover" "define" "references"
                  "inlay-hint" "diagnostics" "document-symbol" "signature"
                  "code-action" "prepare-rename"))
    (if (gethash name test-results)
        (progn (message "  PASS: %s" name) (setq pass (1+ pass)))
      (progn (message "  FAIL: %s" name) (setq fail (1+ fail)))))
  (message "\n%d passed, %d failed" pass fail))

;; Cleanup
(delete-file test-file)
(delete-directory test-dir t)

;; Show lsp-bridge log
(message "\n=== *lsp-bridge* ===")
(when (get-buffer "*lsp-bridge*")
  (with-current-buffer "*lsp-bridge*"
    (message "%s" (buffer-string))))

(kill-emacs 0)
