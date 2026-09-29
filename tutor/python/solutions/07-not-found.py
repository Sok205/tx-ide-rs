# Answer 404 for any path you don't know. The last lines of do_GET become:
        if self.path == "/":
            self.send_text(200, "Welcome to your tx tutor server!\n")
            return
        self.send_text(404, "not found\n")
