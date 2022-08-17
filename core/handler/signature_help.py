from core.handler import Handler
from core.utils import *


class SignatureHelp(Handler):
    name = "signature_help"
    method = "textDocument/signatureHelp"
    cancel_on_change = True
    provider = "signature_help_provider"

    def process_request(self, position) -> dict:
        return dict(position=position)

    def process_response(self, response: dict) -> None:
        if response is None or not len(response["signatures"]):
            return
        signature = response["signatures"][response.get("activeSignature", 0)]
        if signature:
            function_signature = signature.get("label")
            parameters = signature.get("parameters", [])

            arguments = []
            for parameter in parameters:
                label = parameter["label"]
                if isinstance(label, str): # most lsp server return string
                    arguments.append(label)
                elif isinstance(label, list) and len(label) == 2: # ccls return list
                    signatures_label = response["signatures"][0]["label"]
                    arguments.append(signatures_label[label[0]:label[1]])

            eval_in_emacs("lsp-bridge-signature-help-update", function_signature, arguments, response.get("activeParameter", 0))
