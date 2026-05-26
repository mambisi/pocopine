struct Customers;

#[pocopine_sync_crud::resource(name = "customers", schema_version = "two")]
impl Customers {}

fn main() {}
