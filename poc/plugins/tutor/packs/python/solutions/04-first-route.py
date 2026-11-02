# In Handler.do_GET, before the welcome line:
    def do_GET(self):
        if self.path == "/hello":
            self.send_text(200, "hello\n")
            return
        self.send_text(200, "Welcome to your tx tutor server!\n")
