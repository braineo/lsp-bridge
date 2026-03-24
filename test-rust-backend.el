;;; test-rust-backend.el --- Minimal test for Rust backend -*- lexical-binding: t -*-

;; Add all elpa packages to load-path
(let ((elpa-dir (expand-file-name "~/.emacs.d/elpa/")))
  (when (file-directory-p elpa-dir)
    (dolist (pkg-dir (directory-files elpa-dir t "^[^.]"))
      (when (file-directory-p pkg-dir)
        (add-to-list 'load-path pkg-dir)))))

(require 'lsp-bridge)

;; Force Rust backend
(setq lsp-bridge-use-rust-backend t)
;; Use ty for Python (override user config for testing)
(setq lsp-bridge-single-lang-server-mode-list
      '(((python-mode python-ts-mode) . "ty")))

(message "=== Rust backend test ===")
(message "Binary: %s" lsp-bridge-rust-binary)
(message "Exists: %s" (file-exists-p lsp-bridge-rust-binary))

;; Start the EPC server
(message "Starting EPC server...")
(lsp-bridge--start-epc-server)
(message "EPC server port: %s" lsp-bridge-server-port)

;; Start the Rust process
(message "Starting Rust process...")
(lsp-bridge-start-process)

(message "Process prog: %s" lsp-bridge-internal-process-prog)
(message "Process args: %s" lsp-bridge-internal-process-args)
(message "Process live: %s" (process-live-p lsp-bridge-internal-process))

;; Wait for the process to connect back
(message "Waiting for Rust backend to connect...")
(let ((timeout 10)
      (elapsed 0))
  (while (and (not lsp-bridge-epc-process) (< elapsed timeout))
    (accept-process-output nil 1)
    (setq elapsed (1+ elapsed))
    (message "  waiting... %ds" elapsed))

  (if lsp-bridge-epc-process
      (message "SUCCESS: EPC connection established!")
    (message "FAILED: No EPC connection after %ds" timeout)))

;; Show process buffer output
(message "=== *lsp-bridge* buffer ===")
(when (get-buffer "*lsp-bridge*")
  (with-current-buffer "*lsp-bridge*"
    (message "%s" (buffer-string))))

;; If connected, try a simple call
(when lsp-bridge-epc-process
  ;; Create a test project with pyproject.toml so ty works
  (let* ((test-dir (make-temp-file "lsp-bridge-test" t))
         (test-file (expand-file-name "test.py" test-dir)))

    ;; Create pyproject.toml for ty
    (with-temp-file (expand-file-name "pyproject.toml" test-dir)
      (insert "[project]\nname = \"test\"\n"))

    ;; Create Python test file
    (with-temp-file test-file
      (insert "import os\nos.path\n"))

    (message "Test dir: %s" test-dir)
    (message "Test file: %s" test-file)

    ;; Open the file
    (message "Calling open_file...")
    (condition-case err
        (progn
          (lsp-bridge-call-async "open_file" test-file)
          ;; Wait for server to start
          (sleep-for 5)
          (message "open_file done, waiting for server..."))
      (error (message "open_file error: %S" err)))

    ;; Try completion
    (message "Calling try_completion...")
    (condition-case err
        (progn
          (lsp-bridge-call-async "try_completion" test-file
                                 '(:line 1 :character 7)  ;; after "os.path"
                                 "."  ;; trigger char
                                 "path"  ;; prefix
                                 1)  ;; version
          (sleep-for 3)
          (message "try_completion sent"))
      (error (message "try_completion error: %S" err)))

    (delete-file test-file)
    (delete-directory test-dir t)))

(message "=== Test complete ===")

;; Show final process state
(message "Process live: %s" (and lsp-bridge-internal-process (process-live-p lsp-bridge-internal-process)))
(message "EPC process: %s" (if lsp-bridge-epc-process "connected" "nil"))

;; Show lsp-bridge buffer again for any new output
(when (get-buffer "*lsp-bridge*")
  (with-current-buffer "*lsp-bridge*"
    (message "=== Final *lsp-bridge* output ===")
    (message "%s" (buffer-string))))

(kill-emacs 0)
