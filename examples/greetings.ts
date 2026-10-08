class User {
  constructor(public name: string) {}

  rename(name: string): void {
    this.name = name;
  }

  greet(): void {
    echo(`Hello, ${this.name}!`);
  }
}

const user = new User("Twilight");

user.greet();
user.rename("Arch");
user.greet();
