struct Customers;

#[pocopine_sync_crud::resource(name = "tenant-customers")]
impl Customers {}

fn main() {}
