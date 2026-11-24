"""A reply that carries files. It is still a str, so everything that only reads text works unchanged."""


class Reply(str):
    attachments: list

    def __new__(cls, text: str, attachments=()):
        reply = super().__new__(cls, text)
        reply.attachments = list(attachments)
        return reply
