"""A text reply with optional files or a conversation reset."""


class Reply(str):
    attachments: list

    def __new__(cls, text: str, attachments=(), *, reset_context=False):
        reply = super().__new__(cls, text)
        reply.attachments = list(attachments)
        reply.reset_context = reset_context
        return reply
